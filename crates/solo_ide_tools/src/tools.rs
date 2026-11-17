//! The tools: workspace symbols, go to definition, find references and
//! diagnostics, from the language servers of one workspace's project. The
//! logic follows Zed's own agent tools (`agent::tools::go_to_definition_tool`,
//! `find_references_tool`, `diagnostics_tool`, `symbol_locator`), with
//! output in `path:line:column: text` lines like grep's.
//!
//! Read-only, and only about the workspace: paths must be files in its
//! visible worktrees (not through a symlink out of them, not `.env`-like
//! private files), and results elsewhere (e.g. in dependencies) are counted
//! but not shown.

use std::{
    cell::RefCell, collections::VecDeque, fmt::Write as _, path::Path, rc::Rc, time::Duration,
};

use gpui::{App, AsyncApp, Entity};
use language::{
    Buffer, BufferSnapshot, DiagnosticSeverity, OffsetRangeExt as _, Point, ToPoint as _,
};
use project::{
    Project, ProjectPath,
    lsp_store::{OpenLspBufferHandle, SymbolLocation},
};
use serde::Deserialize;
use text::{Bias, Unclipped};
use util::paths::PathWithPosition;

/// How many buffers the tools keep open with their language servers.
const MAX_OPEN_BUFFERS: usize = 32;
/// How long a file's language server gets to start.
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(20);
const SERVER_POLL: Duration = Duration::from_millis(100);
/// How far from the given line a symbol is looked for.
const LINE_SLACK: u32 = 3;
const MAX_LINE_CHARS: usize = 200;
const DEFAULT_SYMBOLS: usize = 50;
const MAX_SYMBOLS: usize = 200;
const MAX_REFERENCES: usize = 300;
const MAX_DIAGNOSTICS: usize = 200;
const MAX_DIAGNOSTIC_FILES: usize = 200;
/// How many same-named symbols `symbol` alone resolves to.
const MAX_NAMED_SYMBOLS: usize = 5;

/// Buffers the tools opened, kept registered with their language servers
/// (which then keep them open) so asking again is fast; most recent last.
pub type OpenBuffers = Rc<RefCell<VecDeque<(ProjectPath, Entity<Buffer>, OpenLspBufferHandle)>>>;

pub struct IdeTools {
    project: Entity<Project>,
    open: OpenBuffers,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolsInput {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Locator {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    column: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticsInput {
    #[serde(default)]
    path: Option<String>,
}

/// A place a tool starts from.
struct Target {
    buffer: Entity<Buffer>,
    point: Point,
    name: String,
}

fn parse<T: for<'de> Deserialize<'de>>(arguments: serde_json::Value) -> Result<T, String> {
    let arguments = if arguments.is_null() {
        serde_json::json!({})
    } else {
        arguments
    };
    serde_json::from_value(arguments).map_err(|error| format!("Invalid arguments: {error}"))
}

impl IdeTools {
    pub fn new(project: Entity<Project>, open: OpenBuffers) -> Self {
        Self { project, open }
    }

    pub async fn call(
        &self,
        name: &str,
        arguments: serde_json::Value,
        cx: &mut AsyncApp,
    ) -> Result<String, String> {
        match name {
            "workspace_symbols" => self.workspace_symbols(parse(arguments)?, cx).await,
            "go_to_definition" => self.go_to_definition(parse(arguments)?, cx).await,
            "find_references" => self.find_references(parse(arguments)?, cx).await,
            "diagnostics" => self.diagnostics(parse(arguments)?, cx).await,
            _ => Err(format!("Unknown tool: {name}")),
        }
    }

    async fn workspace_symbols(
        &self,
        input: SymbolsInput,
        cx: &mut AsyncApp,
    ) -> Result<String, String> {
        let query = input.query.trim();
        if query.is_empty() {
            return Err("Give a `query`.".to_string());
        }
        self.require_some_server(cx)?;
        let limit = input.limit.unwrap_or(DEFAULT_SYMBOLS).clamp(1, MAX_SYMBOLS);
        let symbols = self
            .project
            .update(cx, |project, cx| project.symbols(query, cx))
            .await
            .map_err(|error| format!("Workspace symbol search failed: {error:#}"))?;
        let mut lines = Vec::new();
        let mut elsewhere = 0;
        self.project.read_with(cx, |project, cx| {
            for symbol in &symbols {
                let SymbolLocation::InProject(project_path) = &symbol.path else {
                    elsewhere += 1;
                    continue;
                };
                let Some(path) = display_path(project, project_path, cx) else {
                    elsewhere += 1;
                    continue;
                };
                let start = symbol.range.start.0;
                let mut line = format!(
                    "{:?} {}  {path}:{}:{}",
                    symbol.kind,
                    symbol.name,
                    start.row + 1,
                    start.column + 1
                );
                if let Some(container) = symbol.container_name.as_deref().filter(|c| !c.is_empty())
                {
                    write!(line, "  (in {container})").ok();
                }
                lines.push(line);
            }
        });
        let total = lines.len();
        lines.truncate(limit);
        let mut output = if total == 0 {
            format!("No symbols matching `{query}` in the workspace.")
        } else {
            lines.join("\n")
        };
        if total > limit {
            write!(
                output,
                "\n... and {} more (raise `limit` or narrow `query`)",
                total - limit
            )
            .ok();
        }
        if elsewhere > 0 {
            write!(
                output,
                "\n({elsewhere} more outside the workspace, not shown)"
            )
            .ok();
        }
        Ok(self.with_busy_note(output, false, cx))
    }

    async fn go_to_definition(&self, input: Locator, cx: &mut AsyncApp) -> Result<String, String> {
        let at_place = input.path.is_some();
        let targets = self.locate(&input, cx).await?;
        let name = targets[0].name.clone();
        let mut found = Vec::new();
        let mut elsewhere = 0;
        if at_place {
            for target in &targets {
                let definitions = self
                    .project
                    .update(cx, |project, cx| {
                        project.definitions(&target.buffer, target.point, cx)
                    })
                    .await
                    .map_err(|error| format!("Go to definition failed: {error:#}"))?
                    .unwrap_or_default();
                self.project.read_with(cx, |project, cx| {
                    for link in &definitions {
                        let point = link
                            .target
                            .range
                            .start
                            .to_point(&link.target.buffer.read(cx).snapshot());
                        match location_line(project, &link.target.buffer, point, cx) {
                            Some(line) => found.push(line),
                            None => elsewhere += 1,
                        }
                    }
                });
            }
        } else {
            // The workspace symbols are the definitions.
            self.project.read_with(cx, |project, cx| {
                for target in &targets {
                    if let Some(line) = location_line(project, &target.buffer, target.point, cx) {
                        found.push(line);
                    }
                }
            });
        }
        dedup(&mut found);
        let mut output = match found.len() {
            0 if elsewhere == 0 => format!("No definition found for `{name}`."),
            0 => format!("`{name}` is defined outside the workspace."),
            1 => format!("Definition of `{name}`:\n{}", found[0]),
            count => format!("{count} definitions of `{name}`:\n{}", found.join("\n")),
        };
        if elsewhere > 0 && !found.is_empty() {
            write!(
                output,
                "\n({elsewhere} more outside the workspace, not shown)"
            )
            .ok();
        }
        Ok(self.with_busy_note(output, false, cx))
    }

    async fn find_references(&self, input: Locator, cx: &mut AsyncApp) -> Result<String, String> {
        let targets = self.locate(&input, cx).await?;
        let name = targets[0].name.clone();
        let mut found = Vec::new();
        let mut elsewhere = 0;
        for target in &targets {
            let references = self
                .project
                .update(cx, |project, cx| {
                    project.references(&target.buffer, target.point, cx)
                })
                .await
                .map_err(|error| format!("Find references failed: {error:#}"))?
                .unwrap_or_default();
            self.project.read_with(cx, |project, cx| {
                for location in &references {
                    let point = location
                        .range
                        .start
                        .to_point(&location.buffer.read(cx).snapshot());
                    match location_line(project, &location.buffer, point, cx) {
                        Some(line) => found.push(line),
                        None => elsewhere += 1,
                    }
                }
            });
        }
        dedup(&mut found);
        let total = found.len();
        found.truncate(MAX_REFERENCES);
        let mut output = if total == 0 {
            format!("No references to `{name}` found in the workspace.")
        } else {
            format!(
                "{total} reference{} to `{name}`:\n{}",
                if total == 1 { "" } else { "s" },
                found.join("\n")
            )
        };
        if total > MAX_REFERENCES {
            write!(output, "\n... and {} more", total - MAX_REFERENCES).ok();
        }
        if elsewhere > 0 {
            write!(
                output,
                "\n({elsewhere} more outside the workspace, not shown)"
            )
            .ok();
        }
        Ok(self.with_busy_note(output, false, cx))
    }

    async fn diagnostics(
        &self,
        input: DiagnosticsInput,
        cx: &mut AsyncApp,
    ) -> Result<String, String> {
        let lsp_store = self.project.read_with(cx, |project, _| project.lsp_store());
        match input
            .path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            Some(path) => {
                let project_path = self.project.read_with(cx, |project, cx| {
                    resolve_path(project, &PathWithPosition::parse_str(path).path, cx)
                })?;
                let buffer = self.open_buffer(project_path.clone(), cx).await?;
                let has_server = self.wait_for_servers(&buffer, cx).await;
                if has_server {
                    let pull = lsp_store.update(cx, |lsp_store, cx| {
                        lsp_store.pull_diagnostics_for_buffer(buffer.clone(), cx)
                    });
                    if let Err(error) = pull.await {
                        log::debug!("IDE tools: pulling diagnostics failed: {error:#}");
                    }
                }
                let output = self.project.read_with(cx, |project, cx| {
                    let display = display_path(project, &project_path, cx).unwrap_or_default();
                    let snapshot = buffer.read(cx).snapshot();
                    let mut lines = Vec::new();
                    for (_, group) in snapshot.diagnostic_groups(None) {
                        let entry = &group.entries[group.primary_ix];
                        let severity = match entry.diagnostic.severity {
                            DiagnosticSeverity::ERROR => "error",
                            DiagnosticSeverity::WARNING => "warning",
                            _ => continue,
                        };
                        let start = entry.range.to_point(&snapshot).start;
                        let message = entry.diagnostic.message.as_str().replace('\n', " ");
                        lines.push(format!(
                            "{display}:{}:{}: {severity}: {message}",
                            start.row + 1,
                            char_column(&snapshot, start) + 1
                        ));
                    }
                    let total = lines.len();
                    lines.truncate(MAX_DIAGNOSTICS);
                    let mut output = if total == 0 {
                        format!("No errors or warnings in {display}.")
                    } else {
                        lines.join("\n")
                    };
                    if total > MAX_DIAGNOSTICS {
                        write!(output, "\n... and {} more", total - MAX_DIAGNOSTICS).ok();
                    }
                    if !has_server {
                        output.push_str("\n(No language server checks this file.)");
                    }
                    output
                });
                Ok(self.with_busy_note(output, true, cx))
            }
            None => {
                let pull = lsp_store.update(cx, |lsp_store, cx| {
                    lsp_store.pull_workspace_diagnostics_once(cx)
                });
                pull.await;
                let output = self.project.read_with(cx, |project, cx| {
                    let mut lines = Vec::new();
                    let (mut errors, mut warnings) = (0, 0);
                    let mut by_path = std::collections::BTreeMap::<String, (usize, usize)>::new();
                    for (project_path, _, summary) in project.diagnostic_summaries(false, cx) {
                        if summary.error_count == 0 && summary.warning_count == 0 {
                            continue;
                        }
                        let Some(path) = display_path(project, &project_path, cx) else {
                            continue;
                        };
                        let counts = by_path.entry(path).or_default();
                        counts.0 += summary.error_count;
                        counts.1 += summary.warning_count;
                    }
                    for (path, (error_count, warning_count)) in &by_path {
                        errors += error_count;
                        warnings += warning_count;
                        lines.push(format!(
                            "{path}: {error_count} error(s), {warning_count} warning(s)"
                        ));
                    }
                    if lines.is_empty() {
                        let mut output = "No errors or warnings in the workspace.".to_string();
                        if project.language_server_statuses(cx).next().is_none() {
                            output.push_str(
                                " (No language server is running yet: ask about a file with `path` to start its language server.)",
                            );
                        }
                        return output;
                    }
                    let files = lines.len();
                    lines.truncate(MAX_DIAGNOSTIC_FILES);
                    let mut output = format!(
                        "{errors} error(s), {warnings} warning(s) in {files} file(s):\n{}",
                        lines.join("\n")
                    );
                    if files > MAX_DIAGNOSTIC_FILES {
                        write!(output, "\n... and {} more files", files - MAX_DIAGNOSTIC_FILES).ok();
                    }
                    output
                });
                Ok(self.with_busy_note(output, true, cx))
            }
        }
    }

    /// Where `input` points: the symbol at a place in a file, or the
    /// workspace symbols with that exact name. Never empty.
    async fn locate(&self, input: &Locator, cx: &mut AsyncApp) -> Result<Vec<Target>, String> {
        let symbol = input
            .symbol
            .as_deref()
            .map(str::trim)
            .filter(|symbol| !symbol.is_empty());
        if let Some(path) = input.path.as_deref() {
            let parsed = PathWithPosition::parse_str(path);
            let line = input.line.or(parsed.row).filter(|line| *line > 0).ok_or(
                "Give `line` with `path` (or a path ending in `:line:column`).".to_string(),
            )?;
            let column = input.column.or(parsed.column).filter(|column| *column > 0);
            let project_path = self
                .project
                .read_with(cx, |project, cx| resolve_path(project, &parsed.path, cx))?;
            let buffer = self.open_buffer(project_path, cx).await?;
            if !self.wait_for_servers(&buffer, cx).await {
                return Err(format!("No language server handles {path}."));
            }
            let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
            let row = line - 1;
            if row > snapshot.max_point().row {
                return Err(format!(
                    "Line {line} is past the end of {path} ({} lines).",
                    snapshot.max_point().row + 1
                ));
            }
            let (point, name) = match (column, symbol) {
                (Some(column), _) => {
                    let point = point_at_column(&snapshot, row, column);
                    let name = symbol
                        .map(str::to_string)
                        .unwrap_or_else(|| word_at(&snapshot, point));
                    (point, name)
                }
                (None, Some(symbol)) => {
                    let point = find_near(&snapshot, row, symbol).ok_or_else(|| {
                        format!(
                            "`{symbol}` isn't on line {line} of {path} (or near it). The line is: {}",
                            line_text(&snapshot, row)
                        )
                    })?;
                    (point, symbol.to_string())
                }
                (None, None) => {
                    return Err("Give `symbol` (or `column`) with `path` and `line`.".to_string());
                }
            };
            return Ok(vec![Target {
                buffer,
                point,
                name,
            }]);
        }
        let Some(symbol) = symbol else {
            return Err(
                "Give `symbol`, or `path` and `line` (with `symbol` or `column`).".to_string(),
            );
        };
        self.require_some_server(cx)?;
        let symbols = self
            .project
            .update(cx, |project, cx| project.symbols(symbol, cx))
            .await
            .map_err(|error| format!("Workspace symbol search failed: {error:#}"))?;
        let (exact, others): (Vec<_>, Vec<_>) = symbols
            .into_iter()
            .filter(|found| matches!(found.path, SymbolLocation::InProject(_)))
            .partition(|found| found.name == symbol);
        if exact.is_empty() {
            let similar: Vec<String> = others.iter().take(10).map(|s| s.name.clone()).collect();
            return Err(if similar.is_empty() {
                format!("No symbol named `{symbol}` in the workspace.")
            } else {
                format!(
                    "No symbol named exactly `{symbol}` in the workspace. Similar: {}",
                    similar.join(", ")
                )
            });
        }
        let mut targets = Vec::new();
        for found in exact.into_iter().take(MAX_NAMED_SYMBOLS) {
            let SymbolLocation::InProject(project_path) = &found.path else {
                continue;
            };
            let Ok(project_path) = self
                .project
                .read_with(cx, |project, cx| checked_path(project, project_path, cx))
            else {
                continue;
            };
            let buffer = self.open_buffer(project_path, cx).await?;
            self.wait_for_servers(&buffer, cx).await;
            let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
            let start = snapshot.point_utf16_to_point(
                snapshot.clip_point_utf16(Unclipped(found.range.start.0), Bias::Left),
            );
            let end = snapshot.point_utf16_to_point(
                snapshot.clip_point_utf16(Unclipped(found.range.end.0), Bias::Left),
            );
            // The range may cover the whole item: the name is in it.
            let point = (start.row..=end.row.min(start.row + 50))
                .find_map(|row| {
                    let text = line_text_full(&snapshot, row);
                    let from = if row == start.row {
                        start.column as usize
                    } else {
                        0
                    };
                    let offset = text.get(from..).and_then(|rest| find_word(rest, symbol))?;
                    Some(Point::new(row, (from + offset) as u32))
                })
                .unwrap_or(start);
            targets.push(Target {
                buffer,
                point,
                name: symbol.to_string(),
            });
        }
        if targets.is_empty() {
            return Err(format!("No symbol named `{symbol}` in the workspace."));
        }
        Ok(targets)
    }

    /// Opens a file of the workspace with its language servers.
    async fn open_buffer(
        &self,
        project_path: ProjectPath,
        cx: &mut AsyncApp,
    ) -> Result<Entity<Buffer>, String> {
        {
            let mut open = self.open.borrow_mut();
            if let Some(index) = open.iter().position(|(path, _, _)| *path == project_path) {
                let entry = open.remove(index).expect("index is in range");
                let buffer = entry.1.clone();
                open.push_back(entry);
                return Ok(buffer);
            }
        }
        let buffer = self
            .project
            .update(cx, |project, cx| {
                project.open_buffer(project_path.clone(), cx)
            })
            .await
            .map_err(|error| format!("Couldn't open the file: {error:#}"))?;
        let handle = self.project.update(cx, |project, cx| {
            project.register_buffer_with_language_servers(&buffer, cx)
        });
        let mut open = self.open.borrow_mut();
        open.push_back((project_path, buffer.clone(), handle));
        while open.len() > MAX_OPEN_BUFFERS {
            open.pop_front();
        }
        Ok(buffer)
    }

    /// Waits for a language server to have the buffer open; false if none
    /// does in time, or the file's language has none.
    async fn wait_for_servers(&self, buffer: &Entity<Buffer>, cx: &mut AsyncApp) -> bool {
        let lsp_store = self.project.read_with(cx, |project, _| project.lsp_store());
        let languages = self
            .project
            .read_with(cx, |project, _| project.languages().clone());
        let buffer_id = buffer.read_with(cx, |buffer, _| buffer.remote_id());
        let mut waited = Duration::ZERO;
        loop {
            let ready = lsp_store.read_with(cx, |lsp_store, _| {
                lsp_store
                    .language_server_ids_for_opened_buffer(buffer_id)
                    .is_some_and(|ids| !ids.is_empty())
            });
            if ready {
                return true;
            }
            if waited > SERVER_START_TIMEOUT {
                return false;
            }
            // Once the file's language is known, give up if it has no server.
            if waited >= Duration::from_secs(1) {
                let language = buffer.read_with(cx, |buffer, _| buffer.language().cloned());
                let has_adapters = language
                    .is_some_and(|language| !languages.lsp_adapters(&language.name()).is_empty());
                if !has_adapters {
                    return false;
                }
            }
            cx.background_executor().timer(SERVER_POLL).await;
            waited += SERVER_POLL;
        }
    }

    fn require_some_server(&self, cx: &mut AsyncApp) -> Result<(), String> {
        let any = self.project.read_with(cx, |project, cx| {
            project.language_server_statuses(cx).next().is_some()
        });
        if any {
            Ok(())
        } else {
            Err("No language server is running in this workspace yet. Ask about a file first (go_to_definition or find_references with `path` and `line`, or diagnostics with `path`): that starts its language server.".to_string())
        }
    }

    /// Adds a note when a language server is still busy (e.g. indexing), so
    /// results may be incomplete. Diagnostics also note running checks.
    fn with_busy_note(&self, mut output: String, checks_too: bool, cx: &mut AsyncApp) -> String {
        let busy: Vec<String> = self.project.read_with(cx, |project, cx| {
            project
                .language_server_statuses(cx)
                .filter_map(|(_, status)| {
                    let work = status
                        .pending_work
                        .values()
                        .find(|work| checks_too || !work.is_disk_based_diagnostics_progress)?;
                    let what = [work.title.as_deref(), work.message.as_deref()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" ");
                    Some(if what.is_empty() {
                        status.name.to_string()
                    } else {
                        format!("{} ({what})", status.name)
                    })
                })
                .collect()
        });
        if !busy.is_empty() {
            write!(
                output,
                "\n\nNote: still working: {}. Results may be incomplete; ask again shortly.",
                busy.join(", ")
            )
            .ok();
        }
        output
    }
}

/// The workspace file `path` names (relative to a workspace root, with or
/// without the root's name, or absolute in one).
fn resolve_path(project: &Project, path: &Path, cx: &App) -> Result<ProjectPath, String> {
    let shown = path.display();
    let project_path = project
        .find_project_path(path, cx)
        .ok_or_else(|| format!("{shown} isn't in this workspace."))?;
    checked_path(project, &project_path, cx).map_err(|error| format!("{shown}: {error}"))
}

/// The path if the tools may look at it: a file of a visible worktree, not
/// reached through a symlink out of it, not a private (`.env`-like) file.
fn checked_path(
    project: &Project,
    project_path: &ProjectPath,
    cx: &App,
) -> Result<ProjectPath, String> {
    let visible = project
        .worktree_for_id(project_path.worktree_id, cx)
        .is_some_and(|worktree| worktree.read(cx).is_visible());
    if !visible {
        return Err("not in this workspace".to_string());
    }
    let entry = project
        .entry_for_path(project_path, cx)
        .ok_or("no such file in this workspace")?;
    if !entry.is_file() {
        return Err("not a file".to_string());
    }
    if entry.is_external {
        return Err("outside the workspace (through a symlink)".to_string());
    }
    if entry.is_private {
        return Err("a private file the IDE tools don't read".to_string());
    }
    Ok(ProjectPath {
        worktree_id: project_path.worktree_id,
        path: entry.path.clone(),
    })
}

/// How a workspace path is shown: relative to its root, and with the root's
/// name when the workspace has several. `None` if it's not one the tools
/// may show.
fn display_path(project: &Project, project_path: &ProjectPath, cx: &App) -> Option<String> {
    let project_path = checked_path(project, project_path, cx).ok()?;
    let path = project_path.path.as_unix_str();
    if project.visible_worktrees(cx).nth(1).is_some() {
        let worktree = project.worktree_for_id(project_path.worktree_id, cx)?;
        Some(format!(
            "{}/{path}",
            worktree.read(cx).root_name().as_unix_str()
        ))
    } else {
        Some(path.to_string())
    }
}

/// `path:line:column: text` for a place in a buffer of the workspace.
fn location_line(
    project: &Project,
    buffer: &Entity<Buffer>,
    point: Point,
    cx: &App,
) -> Option<String> {
    let buffer = buffer.read(cx);
    let file = buffer.file()?;
    let project_path = ProjectPath {
        worktree_id: file.worktree_id(cx),
        path: file.path().clone(),
    };
    let path = display_path(project, &project_path, cx)?;
    let snapshot = buffer.snapshot();
    Some(format!(
        "{path}:{}:{}: {}",
        point.row + 1,
        char_column(&snapshot, point) + 1,
        line_text(&snapshot, point.row)
    ))
}

fn line_text_full(snapshot: &BufferSnapshot, row: u32) -> String {
    snapshot
        .text_for_range(Point::new(row, 0)..Point::new(row, snapshot.line_len(row)))
        .collect()
}

/// The line, trimmed and cut short.
fn line_text(snapshot: &BufferSnapshot, row: u32) -> String {
    let text = line_text_full(snapshot, row);
    let text = text.trim();
    let mut shown: String = text.chars().take(MAX_LINE_CHARS).collect();
    if shown.len() < text.len() {
        shown.push_str("...");
    }
    shown
}

/// The 0-based column of `point` in characters.
fn char_column(snapshot: &BufferSnapshot, point: Point) -> usize {
    let text = line_text_full(snapshot, point.row);
    let end = (point.column as usize).min(text.len());
    text.get(..end).map_or(end, |before| before.chars().count())
}

/// The point at a 1-based column in characters.
fn point_at_column(snapshot: &BufferSnapshot, row: u32, column: u32) -> Point {
    let text = line_text_full(snapshot, row);
    let byte = text
        .char_indices()
        .nth(column as usize - 1)
        .map_or(text.len(), |(index, _)| index);
    Point::new(row, byte as u32)
}

fn is_identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier at `point`, for messages.
fn word_at(snapshot: &BufferSnapshot, point: Point) -> String {
    let text = line_text_full(snapshot, point.row);
    let at = (point.column as usize).min(text.len());
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_identifier(*c))
        .last()
        .map_or(at, |(index, _)| index);
    let end = text[at..]
        .char_indices()
        .find(|(_, c)| !is_identifier(*c))
        .map_or(text.len(), |(index, _)| at + index);
    if start == end {
        "the symbol".to_string()
    } else {
        text[start..end].to_string()
    }
}

/// Where `word` is in `line`: as a whole identifier if it is anywhere, else
/// its first occurrence.
fn find_word(line: &str, word: &str) -> Option<usize> {
    if word.is_empty() {
        return None;
    }
    let mut first = None;
    for (index, _) in line.match_indices(word) {
        let before = line[..index].chars().next_back();
        let after = line[index + word.len()..].chars().next();
        if !before.is_some_and(is_identifier) && !after.is_some_and(is_identifier) {
            return Some(index);
        }
        first.get_or_insert(index);
    }
    first
}

/// `symbol` on `row`, or the nearest line around it with it (line numbers
/// from an older read of the file may be off by a few).
fn find_near(snapshot: &BufferSnapshot, row: u32, symbol: &str) -> Option<Point> {
    let max_row = snapshot.max_point().row;
    let rows = std::iter::once(row).chain((1..=LINE_SLACK).flat_map(|delta| {
        [
            row.checked_sub(delta),
            Some(row + delta).filter(|row| *row <= max_row),
        ]
        .into_iter()
        .flatten()
    }));
    for row in rows {
        if let Some(offset) = find_word(&line_text_full(snapshot, row), symbol) {
            return Some(Point::new(row, offset as u32));
        }
    }
    None
}

fn dedup(lines: &mut Vec<String>) {
    let mut seen = collections::HashSet::default();
    lines.retain(|line| seen.insert(line.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;
    use gpui::TestAppContext;
    use language::{FakeLspAdapter, Language, LanguageConfig, LanguageMatcher};
    use project::FakeFs;
    use serde_json::json;
    use settings::SettingsStore;
    use std::sync::Arc;
    use util::path;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
        });
    }

    fn rust() -> Arc<Language> {
        Arc::new(Language::new(
            LanguageConfig {
                name: "Rust".into(),
                matcher: LanguageMatcher {
                    path_suffixes: vec!["rs".into()],
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            },
            None,
        ))
    }

    const LIB: &str = "pub fn helper(x: u32) -> u32 {\n    x + 1\n}\n";
    const MAIN: &str = "use crate::lib::helper;\n\nfn main() {\n    let y = helper(1);\n    println!(\"{}\", helper(y));\n}\n";

    fn uri(path: &str) -> lsp::Uri {
        lsp::Uri::from_file_path(path).unwrap()
    }

    fn range(line: u32, start: u32, end: u32) -> lsp::Range {
        lsp::Range::new(
            lsp::Position::new(line, start),
            lsp::Position::new(line, end),
        )
    }

    /// A project with two Rust files, a private file, and a file outside it,
    /// served by a fake language server that knows where `helper` is.
    async fn project(cx: &mut TestAppContext) -> (Entity<Project>, lsp::FakeLanguageServer) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "ws": {
                    "src": { "lib.rs": LIB, "main.rs": MAIN },
                    ".env": "SECRET=1",
                    "notes.txt": "helper",
                },
                "outside": { "dep.rs": "pub fn dep() {}\n" },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root/ws").as_ref()], cx).await;
        let languages = project.read_with(cx, |project, _| project.languages().clone());
        languages.add(rust());
        let mut servers = languages.register_fake_lsp(
            "Rust",
            FakeLspAdapter {
                capabilities: lsp::ServerCapabilities {
                    definition_provider: Some(lsp::OneOf::Left(true)),
                    references_provider: Some(lsp::OneOf::Left(true)),
                    workspace_symbol_provider: Some(lsp::OneOf::Left(true)),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // Start the server, as a file open in Solo would.
        let (_buffer, handle) = project
            .update(cx, |project, cx| {
                project.open_local_buffer_with_lsp(path!("/root/ws/src/main.rs"), cx)
            })
            .await
            .unwrap();
        std::mem::forget(handle);
        let server = servers.next().await.unwrap();
        cx.run_until_parked();

        let lib = path!("/root/ws/src/lib.rs");
        let main = path!("/root/ws/src/main.rs");
        let dep = path!("/root/outside/dep.rs");
        server.set_request_handler::<lsp::request::GotoDefinition, _, _>(
            move |params, _| async move {
                let position = params.text_document_position_params.position;
                // `helper` on main.rs's line 4 (0-based 3), or `dep`.
                Ok(match position.line {
                    3 => Some(lsp::GotoDefinitionResponse::Array(vec![
                        lsp::Location::new(uri(lib), range(0, 7, 13)),
                        lsp::Location::new(uri(dep), range(0, 7, 10)),
                    ])),
                    _ => None,
                })
            },
        );
        server.set_request_handler::<lsp::request::References, _, _>(move |params, _| async move {
            let position = params.text_document_position.position;
            let document = params.text_document_position.text_document.uri;
            assert!(
                (document == uri(main) && position == lsp::Position::new(3, 12))
                    || (document == uri(lib) && position == lsp::Position::new(0, 7)),
                "references asked at {document:?} {position:?}"
            );
            Ok(Some(vec![
                lsp::Location::new(uri(lib), range(0, 7, 13)),
                lsp::Location::new(uri(main), range(3, 12, 18)),
                lsp::Location::new(uri(main), range(4, 19, 25)),
                lsp::Location::new(uri(dep), range(0, 0, 3)),
            ]))
        });
        server.set_request_handler::<lsp::WorkspaceSymbolRequest, _, _>(
            move |params, _| async move {
                #[allow(deprecated)]
                let symbol = |name: &str, path: &str, range: lsp::Range| lsp::SymbolInformation {
                    name: name.to_string(),
                    kind: lsp::SymbolKind::FUNCTION,
                    tags: None,
                    deprecated: None,
                    location: lsp::Location::new(uri(path), range),
                    container_name: None,
                };
                let all = [
                    // Whole items, as rust-analyzer gives them.
                    symbol(
                        "helper",
                        lib,
                        lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(2, 1)),
                    ),
                    symbol("helper_two", lib, range(0, 0, 1)),
                    symbol("dep", dep, range(0, 0, 15)),
                ];
                Ok(Some(lsp::WorkspaceSymbolResponse::Flat(
                    all.into_iter()
                        .filter(|symbol| symbol.name.contains(&params.query))
                        .collect(),
                )))
            },
        );
        (project, server)
    }

    async fn call(
        project: &Entity<Project>,
        name: &str,
        arguments: serde_json::Value,
        cx: &mut TestAppContext,
    ) -> Result<String, String> {
        use futures::FutureExt as _;
        let tools = IdeTools::new(project.clone(), OpenBuffers::default());
        let name = name.to_string();
        let mut task =
            cx.spawn(|mut cx| async move { tools.call(&name, arguments, &mut cx).await });
        // Waiting for a language server polls with a timer.
        for _ in 0..1000 {
            cx.run_until_parked();
            if let Some(result) = (&mut task).now_or_never() {
                return result;
            }
            cx.executor().advance_clock(SERVER_POLL);
        }
        panic!("the tool didn't finish");
    }

    #[gpui::test]
    async fn test_workspace_symbols(cx: &mut TestAppContext) {
        let (project, _server) = project(cx).await;
        let output = call(
            &project,
            "workspace_symbols",
            json!({"query": "helper"}),
            cx,
        )
        .await
        .unwrap();
        assert_eq!(
            output,
            "Function helper  src/lib.rs:1:1\nFunction helper_two  src/lib.rs:1:1"
        );
        let output = call(&project, "workspace_symbols", json!({"query": "dep"}), cx)
            .await
            .unwrap();
        assert!(output.starts_with("No symbols matching `dep` in the workspace."));
        assert!(output.contains("1 more outside the workspace"));
        assert!(!output.contains("dep.rs"));
        let output = call(
            &project,
            "workspace_symbols",
            json!({"query": "helper", "limit": 1}),
            cx,
        )
        .await
        .unwrap();
        assert!(output.contains("... and 1 more"));
        assert!(
            call(&project, "workspace_symbols", json!({}), cx)
                .await
                .is_err()
        );
    }

    #[gpui::test]
    async fn test_go_to_definition(cx: &mut TestAppContext) {
        let (project, _server) = project(cx).await;
        // At a place: the symbol on the line, or a line off by one.
        for arguments in [
            json!({"path": "src/main.rs", "line": 4, "symbol": "helper"}),
            json!({"path": "src/main.rs", "line": 3, "symbol": "helper"}),
            json!({"path": "src/main.rs:4:13"}),
            json!({"path": path!("/root/ws/src/main.rs"), "line": 4, "column": 13}),
        ] {
            let output = call(&project, "go_to_definition", arguments.clone(), cx)
                .await
                .unwrap_or_else(|error| panic!("{arguments}: {error}"));
            assert!(
                output.contains("src/lib.rs:1:8: pub fn helper(x: u32) -> u32 {"),
                "{arguments}: {output}"
            );
            assert!(output.contains("(1 more outside the workspace, not shown)"));
            assert!(!output.contains("dep.rs"));
        }
        // By name alone, from the workspace symbols.
        let output = call(
            &project,
            "go_to_definition",
            json!({"symbol": "helper"}),
            cx,
        )
        .await
        .unwrap();
        assert_eq!(
            output,
            "Definition of `helper`:\nsrc/lib.rs:1:8: pub fn helper(x: u32) -> u32 {"
        );
        let error = call(&project, "go_to_definition", json!({"symbol": "help"}), cx)
            .await
            .unwrap_err();
        assert!(error.contains("Similar: helper"), "{error}");
        let error = call(
            &project,
            "go_to_definition",
            json!({"path": "src/main.rs", "line": 4, "symbol": "nothing"}),
            cx,
        )
        .await
        .unwrap_err();
        assert!(error.contains("let y = helper(1);"), "{error}");
    }

    #[gpui::test]
    async fn test_find_references(cx: &mut TestAppContext) {
        let (project, _server) = project(cx).await;
        let expected = "3 references to `helper`:\n\
            src/lib.rs:1:8: pub fn helper(x: u32) -> u32 {\n\
            src/main.rs:4:13: let y = helper(1);\n\
            src/main.rs:5:20: println!(\"{}\", helper(y));\n\
            (1 more outside the workspace, not shown)";
        let output = call(
            &project,
            "find_references",
            json!({"path": "src/main.rs", "line": 4, "symbol": "helper"}),
            cx,
        )
        .await
        .unwrap();
        assert_eq!(output, expected);
        // By name: asked at the name in the item, not at the item's start.
        let output = call(&project, "find_references", json!({"symbol": "helper"}), cx)
            .await
            .unwrap();
        assert_eq!(output, expected);
    }

    #[gpui::test]
    async fn test_diagnostics(cx: &mut TestAppContext) {
        let (project, server) = project(cx).await;
        let lib = path!("/root/ws/src/lib.rs");
        // Open it first, so the server has it.
        call(&project, "diagnostics", json!({"path": "src/lib.rs"}), cx)
            .await
            .unwrap();
        server.notify::<lsp::notification::PublishDiagnostics>(lsp::PublishDiagnosticsParams {
            uri: uri(lib),
            version: None,
            diagnostics: vec![
                lsp::Diagnostic {
                    range: range(1, 4, 5),
                    severity: Some(lsp::DiagnosticSeverity::ERROR),
                    message: lsp::DiagnosticMessage::String(
                        "mismatched types\nexpected u32".to_string(),
                    ),
                    ..Default::default()
                },
                lsp::Diagnostic {
                    range: range(0, 7, 13),
                    severity: Some(lsp::DiagnosticSeverity::WARNING),
                    message: lsp::DiagnosticMessage::String("unused".to_string()),
                    ..Default::default()
                },
                lsp::Diagnostic {
                    range: range(0, 0, 1),
                    severity: Some(lsp::DiagnosticSeverity::HINT),
                    message: lsp::DiagnosticMessage::String("a hint".to_string()),
                    ..Default::default()
                },
            ],
        });
        cx.run_until_parked();
        let output = call(&project, "diagnostics", json!({"path": "src/lib.rs"}), cx)
            .await
            .unwrap();
        assert_eq!(
            output,
            "src/lib.rs:1:8: warning: unused\nsrc/lib.rs:2:5: error: mismatched types expected u32"
        );
        let output = call(&project, "diagnostics", json!({}), cx).await.unwrap();
        assert_eq!(
            output,
            "1 error(s), 1 warning(s) in 1 file(s):\nsrc/lib.rs: 1 error(s), 1 warning(s)"
        );
        let output = call(&project, "diagnostics", json!({"path": "src/main.rs"}), cx)
            .await
            .unwrap();
        assert_eq!(output, "No errors or warnings in src/main.rs.");
    }

    #[gpui::test]
    async fn test_only_workspace_files(cx: &mut TestAppContext) {
        let (project, _server) = project(cx).await;
        for (arguments, error) in [
            (
                json!({"path": path!("/root/outside/dep.rs")}),
                "isn't in this workspace",
            ),
            (
                json!({"path": "../outside/dep.rs"}),
                "isn't in this workspace",
            ),
            (json!({"path": ".env"}), "private file"),
            (json!({"path": "src"}), "not a file"),
            (json!({"path": "src/missing.rs"}), "isn't in this workspace"),
            (json!({"path": "ws/src/missing.rs"}), "no such file"),
        ] {
            let result = call(&project, "diagnostics", arguments.clone(), cx).await;
            let message = result.expect_err(&arguments.to_string());
            assert!(message.contains(error), "{arguments}: {message}");
        }
        // A file no language server handles: given up on once its language
        // is known.
        let error = call(
            &project,
            "go_to_definition",
            json!({"path": "notes.txt", "line": 1, "symbol": "helper"}),
            cx,
        )
        .await
        .unwrap_err();
        assert!(error.contains("No language server"), "{error}");
        let error = call(&project, "go_to_definition", json!({"line": 3}), cx)
            .await
            .unwrap_err();
        assert!(error.contains("Give `symbol`"), "{error}");
        let error = call(&project, "go_to_definition", json!({"bogus": 1}), cx)
            .await
            .unwrap_err();
        assert!(error.contains("Invalid arguments"), "{error}");
    }

    #[test]
    fn finds_whole_words_first() {
        assert_eq!(find_word("helpers(helper)", "helper"), Some(8));
        assert_eq!(find_word("helpers()", "helper"), Some(0));
        assert_eq!(find_word("x", ""), None);
    }
}
