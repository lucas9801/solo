//! SOLO: path links measured on agent output: each entry is a line an agent
//! wrote with a file reference in it, the folder the agent worked in, and
//! the files on disk the reference may mean. The line is rendered the way
//! Claude Code shows it in terminals 80, 100 and 120 columns wide; every
//! character of the reference must be a link, and the link must resolve to
//! the expected file and line.
//!
//! The entries committed here are made up, with folder structures to match.
//! A real corpus (`corpus.jsonl`, one entry per line, never committed: it
//! holds private project paths) is read from the path in the
//! `SOLO_LINK_CORPUS` environment variable, and its pass rates printed by
//! kind (written as JSON to `SOLO_LINK_CORPUS_REPORT` if set):
//!
//! ```text
//! SOLO_LINK_CORPUS=E:\solo-link-corpus\corpus.jsonl cargo test -p terminal_view link_corpus -- --nocapture
//! ```

use crate::terminal_path_like_target::{LinkPurpose, PathLinkResolution, resolve_path_like_target};
use collections::{BTreeMap, HashMap};
use gpui::{TestAppContext, VisualTestContext, WeakEntity};
use project::{Fs as _, Project};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use terminal::{PathLikeTarget, link_probe::LinkProbe};
use util::paths::PathWithPosition;
use workspace::{AppState, MultiWorkspace, Workspace, path_link::BackgroundPathChecks};

/// What the reference should resolve to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expected {
    /// The one file of its name.
    Unique,
    /// One of several files of its name: the expected one first, or the
    /// link marked as a guess (for the link menu to ask).
    Ambiguous,
    /// A file only another project has: found in another tab.
    InOtherProject,
}

#[derive(Clone, Debug)]
struct CorpusEntry {
    text: String,
    reference: String,
    working_directory: PathBuf,
    /// The files the reference may mean; the expected one first.
    candidates: Vec<PathBuf>,
    expected: Expected,
    kinds: Vec<String>,
}

/// The first of `keys` that `value` has, as text.
fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key)?.as_str().map(str::to_string))
}

impl CorpusEntry {
    /// An entry of `corpus.jsonl`. The field names are those of the
    /// corpus's README.txt as far as known, with likely alternatives.
    fn parse(value: &Value) -> Option<Self> {
        let text = text_field(value, &["text", "line", "raw", "context", "original"])?;
        let reference = text_field(value, &["ref", "reference", "mention", "match"])?;
        let working_directory = PathBuf::from(text_field(
            value,
            &["cwd", "agent_cwd", "working_directory", "project_dir"],
        )?);
        let mut candidates = value
            .get("candidates")?
            .as_array()?
            .iter()
            .filter_map(|candidate| {
                candidate
                    .as_str()
                    .or_else(|| candidate.get("path")?.as_str())
                    .map(PathBuf::from)
            })
            .collect::<Vec<_>>();
        if let Some(expected) = text_field(value, &["expected", "answer", "target"]) {
            let expected = PathBuf::from(expected);
            candidates.retain(|candidate| *candidate != expected);
            candidates.insert(0, expected);
        }
        let expected = match text_field(value, &["result"])?.as_str() {
            "unique" => Expected::Unique,
            "ambiguous" => Expected::Ambiguous,
            "in_other_project" => Expected::InOtherProject,
            _ => return None,
        };
        let kinds = match value.get("kinds").or_else(|| value.get("kind")) {
            Some(Value::Array(kinds)) => kinds
                .iter()
                .filter_map(|kind| kind.as_str().map(str::to_string))
                .collect(),
            Some(Value::String(kind)) => vec![kind.clone()],
            _ => Vec::new(),
        };
        Some(Self {
            text,
            reference,
            working_directory,
            candidates,
            expected,
            kinds,
        })
    }

    /// The line the reference names, if any.
    fn row(&self) -> Option<u32> {
        PathWithPosition::parse_str(without_range_end(&self.reference).trim_start_matches(':'))
            .row
    }
}

/// How wide a character is in a terminal.
fn width(character: char) -> usize {
    if ('\u{1100}'..='\u{115F}').contains(&character)
        || ('\u{2E80}'..='\u{A4CF}').contains(&character)
        || ('\u{AC00}'..='\u{D7A3}').contains(&character)
        || ('\u{F900}'..='\u{FAFF}').contains(&character)
        || ('\u{FE30}'..='\u{FE4F}').contains(&character)
        || ('\u{FF00}'..='\u{FF60}').contains(&character)
        || ('\u{FFE0}'..='\u{FFE6}').contains(&character)
    {
        2
    } else {
        1
    }
}

/// `text` the way Claude Code shows it in a terminal `columns` wide: without
/// markdown's backticks and bold marks, broken at spaces (or anywhere in a
/// word too long for a line, and between Chinese characters), a list item's
/// further lines indented by two.
fn render_like_claude_code(text: &str, columns: usize) -> Vec<String> {
    let text = text.replace("**", "").replace('`', "");
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let trimmed = paragraph.trim_start();
        let is_list_item = trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed
                .split_once(". ")
                .is_some_and(|(number, _)| number.chars().all(|c| c.is_ascii_digit()));
        let indent = paragraph.len() - trimmed.len() + if is_list_item { 2 } else { 0 };
        let mut line = String::new();
        let mut line_width = 0;
        // Words: runs of non-space characters, a Chinese character each.
        let mut words: Vec<String> = Vec::new();
        let mut word = String::new();
        for character in paragraph.chars() {
            if character == ' ' {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                words.push(" ".to_string());
            } else if width(character) == 2 {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                words.push(character.to_string());
            } else {
                word.push(character);
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
        for word in words {
            let word_width = word.chars().map(width).sum::<usize>();
            if line_width + word_width > columns && line_width > indent {
                lines.push(std::mem::take(&mut line).trim_end().to_string());
                line = " ".repeat(indent);
                line_width = indent;
                if word == " " {
                    continue;
                }
            }
            for character in word.chars() {
                let character_width = width(character);
                if line_width + character_width > columns {
                    lines.push(std::mem::take(&mut line));
                    line = " ".repeat(indent);
                    line_width = indent;
                }
                line.push(character);
                line_width += character_width;
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}

/// Where each character of `reference` is on the screen: the rendered rows
/// read as one text without the spaces at their ends and starts (a path
/// broken across lines goes on after the next line's indentation).
fn reference_cells(probe: &LinkProbe, reference: &str) -> Option<Vec<(i32, usize)>> {
    let mut text = Vec::new();
    for (line, row) in probe.rows().into_iter().enumerate() {
        let start = row.iter().position(|(_, c)| *c != ' ').unwrap_or(row.len());
        let end = row
            .iter()
            .rposition(|(_, c)| *c != ' ')
            .map_or(start, |end| end + 1);
        for (column, character) in &row[start..end.max(start)] {
            text.push((*character, line as i32, *column));
        }
        // Rows are joined without a space; one still separates words on a
        // row.
    }
    let reference = reference.chars().collect::<Vec<_>>();
    let start = (0..text.len().saturating_sub(reference.len() - 1)).find(|start| {
        reference
            .iter()
            .enumerate()
            .all(|(ix, character)| text[start + ix].0 == *character)
    })?;
    Some(
        text[start..start + reference.len()]
            .iter()
            .map(|(_, line, column)| (*line, *column))
            .collect(),
    )
}

/// `text` without the end of a line range (`-34` of `a.h:12-34`).
fn without_range_end(text: &str) -> &str {
    let digits = text.trim_end_matches(|c: char| c.is_ascii_digit());
    match digits.strip_suffix('-') {
        Some(rest) if digits.len() < text.len() && rest.ends_with(|c: char| c.is_ascii_digit()) => {
            rest
        }
        _ => text,
    }
}

/// The file name a link or reference ends with, without a position.
fn file_name(text: &str) -> String {
    let text = without_range_end(text);
    let path = PathWithPosition::parse_str(text.trim_start_matches(':')).path;
    path.to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

/// What went wrong with an entry, or `None` when it passed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Failure {
    /// The reference isn't where the rendered text was looked for it.
    NotRendered,
    /// Some character of the reference isn't a link (to it).
    NoLink { character: usize, found: Option<String> },
    /// The link resolves to nothing.
    Unresolved { link: String },
    /// The link opens another file (or line).
    WrongTarget { link: String, opened: String },
}

/// The folders tabs are made of: each entry's working directory, unless
/// inside another one.
fn project_roots(entries: &[CorpusEntry]) -> Vec<PathBuf> {
    let mut roots = entries
        .iter()
        .map(|entry| entry.working_directory.clone())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    let all = roots.clone();
    roots.retain(|root| {
        !all.iter()
            .any(|other| other != root && root.starts_with(other))
    });
    roots
}

struct Harness {
    cx: VisualTestContext,
    /// A tab per project root.
    workspaces: Vec<(PathBuf, WeakEntity<Workspace>)>,
}

impl Harness {
    /// A window with a tab per project the entries are in, their candidate
    /// files on a fake file system with enough lines each.
    async fn new(cx: &mut TestAppContext, entries: &[CorpusEntry]) -> Self {
        let fs = cx.update(AppState::test).fs.as_fake().clone();
        cx.update(|cx| {
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let mut lines = HashMap::<PathBuf, u32>::default();
        for entry in entries {
            for candidate in &entry.candidates {
                let needed = lines.entry(candidate.clone()).or_default();
                *needed = (*needed).max(entry.row().unwrap_or(1) + 1);
            }
            fs.create_dir(&entry.working_directory).await.ok();
        }
        for (path, count) in lines {
            if let Some(parent) = path.parent() {
                fs.create_dir(parent).await.ok();
            }
            // A reference without an extension is to a folder.
            if path.extension().is_none() {
                fs.create_dir(&path).await.ok();
            } else {
                fs.insert_file(&path, "x\n".repeat(count as usize).into_bytes())
                    .await;
            }
        }
        let roots = project_roots(entries);
        // Each project a git repository (outside one, folders deeper than
        // `file_scan_depth` aren't indexed), unless SOLO_LINK_CORPUS_GIT=0.
        if std::env::var("SOLO_LINK_CORPUS_GIT").as_deref() != Ok("0") {
            for root in &roots {
                fs.create_dir(&root.join(".git")).await.ok();
            }
        }
        let mut projects = Vec::new();
        for root in &roots {
            projects.push(Project::test(fs.clone(), [root.as_path()], cx).await);
        }
        let first = projects.first().cloned().expect("entries have projects");
        let (multi_workspace, visual) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(first, window, cx));
        let visual = visual.clone();
        let mut harness = Self {
            cx: visual,
            workspaces: Vec::new(),
        };
        let first_workspace =
            multi_workspace.read_with(&harness.cx, |mw, _| mw.workspace().downgrade());
        harness.workspaces.push((roots[0].clone(), first_workspace));
        for (root, project) in roots.iter().zip(projects).skip(1) {
            let workspace = multi_workspace.update_in(&mut harness.cx, |mw, window, cx| {
                mw.test_add_workspace(project, window, cx)
            });
            harness
                .workspaces
                .push((root.clone(), workspace.downgrade()));
        }
        harness.cx.run_until_parked();
        harness
    }

    fn workspace_for(&self, working_directory: &Path) -> WeakEntity<Workspace> {
        self.workspaces
            .iter()
            .find(|(root, _)| working_directory.starts_with(root))
            .map(|(_, workspace)| workspace.clone())
            .expect("a tab for every working directory")
    }

    /// Checks `entry` in a terminal `columns` wide.
    async fn check(&mut self, entry: &CorpusEntry, columns: usize) -> Option<Failure> {
        let lines = render_like_claude_code(&entry.text, columns);
        let mut probe = LinkProbe::new(&lines, columns);
        let Some(cells) = reference_cells(&probe, &entry.reference) else {
            return Some(Failure::NotRendered);
        };
        let wanted_name = file_name(&entry.reference);
        for (character, (line, column)) in cells.iter().enumerate() {
            let link = probe.link_at(*line, *column, false);
            let to_reference = link.as_ref().is_some_and(|link| {
                !link.is_url
                    && (file_name(&link.text) == wanted_name || entry.reference.starts_with(':'))
            });
            if !to_reference {
                return Some(Failure::NoLink {
                    character,
                    found: link.map(|link| link.text),
                });
            }
        }
        let (line, column) = cells[0];
        let link = probe.link_at(line, column, true)?;
        let target = PathLikeTarget {
            maybe_path: link.text.clone(),
            working_directory: Some(entry.working_directory.clone()),
            context_paths: link.context_paths.clone(),
            position_files: link.position_files.clone(),
        };
        let workspace = self.workspace_for(&entry.working_directory);
        let resolution = self
            .cx
            .update(|_, cx| {
                resolve_path_like_target(
                    &workspace,
                    &target,
                    LinkPurpose::Click,
                    cx,
                    BackgroundPathChecks::LocalFileSystem,
                )
            })
            .await;
        let expected = entry.candidates.first()?;
        let row = entry.row();
        let opened = |target: &workspace::path_link::OpenTarget| {
            format!("{}:{:?}", target.path().path.display(), target.path().row)
        };
        let is_expected = |target: &workspace::path_link::OpenTarget| {
            target.path().path == *expected && (row.is_none() || target.path().row == row)
        };
        match (entry.expected, resolution) {
            (_, None) => Some(Failure::Unresolved { link: link.text }),
            (Expected::Unique, Some(PathLinkResolution::Here(resolved)))
                if is_expected(&resolved.target) =>
            {
                None
            }
            (Expected::Ambiguous, Some(PathLinkResolution::Here(resolved)))
                if is_expected(&resolved.target) || resolved.ambiguous =>
            {
                None
            }
            (Expected::InOtherProject, Some(PathLinkResolution::InOtherTabs(found)))
                if found
                    .iter()
                    .any(|found| entry.candidates.contains(&found.target.path().path)) =>
            {
                None
            }
            (_, Some(PathLinkResolution::Here(resolved))) => Some(Failure::WrongTarget {
                link: link.text,
                opened: opened(&resolved.target),
            }),
            (_, Some(PathLinkResolution::InOtherTabs(found))) => Some(Failure::WrongTarget {
                link: link.text,
                opened: found
                    .first()
                    .map(|found| opened(&found.target))
                    .unwrap_or_default(),
            }),
        }
    }
}

/// Pass counts by kind (and "all", and the expected result).
#[derive(Default)]
struct PassRates(BTreeMap<String, (usize, usize)>);

impl PassRates {
    fn record(&mut self, entry: &CorpusEntry, passed: bool) {
        let result = format!("result:{:?}", entry.expected);
        for kind in entry
            .kinds
            .iter()
            .map(String::as_str)
            .chain(["all", result.as_str()])
        {
            let (passes, total) = self.0.entry(kind.to_string()).or_default();
            *passes += usize::from(passed);
            *total += 1;
        }
    }

    fn report(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(kind, (passes, total))| {
                    (
                        kind.clone(),
                        json!({
                            "passed": passes,
                            "total": total,
                            "rate": *passes as f64 / (*total).max(1) as f64,
                        }),
                    )
                })
                .collect(),
        )
    }
}

const WIDTHS: [usize; 3] = [80, 100, 120];

async fn run_corpus(
    cx: &mut TestAppContext,
    entries: &[CorpusEntry],
) -> (PassRates, Vec<(usize, usize, Failure)>) {
    let mut harness = Harness::new(cx, entries).await;
    let mut rates = PassRates::default();
    let mut failures = Vec::new();
    for (ix, entry) in entries.iter().enumerate() {
        let mut passed = true;
        for columns in WIDTHS {
            if let Some(failure) = harness.check(entry, columns).await {
                failures.push((ix, columns, failure));
                passed = false;
            }
        }
        rates.record(entry, passed);
    }
    (rates, failures)
}

fn synthetic_entry(
    text: &str,
    reference: &str,
    working_directory: &str,
    candidates: &[&str],
    expected: Expected,
    kind: &str,
) -> CorpusEntry {
    CorpusEntry {
        text: text.to_string(),
        reference: reference.to_string(),
        working_directory: PathBuf::from(working_directory),
        candidates: candidates.iter().map(PathBuf::from).collect(),
        expected,
        kinds: vec![kind.to_string()],
    }
}

/// Made-up agent answers with the ways references are written in the real
/// ones: only a file name, line ranges, Chinese punctuation around paths
/// and directories, `、` lists, long lines broken by the terminal.
fn synthetic_corpus() -> Vec<CorpusEntry> {
    use util::path;
    let godot = path!("/work/godot_engine");
    let engine = path!("/work/Engine");
    vec![
        synthetic_entry(
            "已经修好了，见 object.cpp:1838-1876、core_bind.cpp:1854-1876 两处。",
            "core_bind.cpp:1854-1876",
            godot,
            &[path!("/work/godot_engine/core/core_bind.cpp")],
            Expected::Unique,
            "line_range",
        ),
        synthetic_entry(
            "已经修好了，见 object.cpp:1838-1876、core_bind.cpp:1854-1876 两处。",
            "object.cpp:1838-1876",
            godot,
            &[path!("/work/godot_engine/core/object/object.cpp")],
            Expected::Unique,
            "line_range",
        ),
        synthetic_entry(
            "改在 _append_bone，dynamic_bone_3d.cpp:152 那里加了判断。",
            "dynamic_bone_3d.cpp:152",
            godot,
            &[path!("/work/godot_engine/modules/dynamicbone/dynamic_bone_3d.cpp")],
            Expected::Unique,
            "chinese_punctuation",
        ),
        synthetic_entry(
            "工具在 modules/tcframework/editor/renderdoc_helper，但还没接上。",
            "modules/tcframework/editor/renderdoc_helper",
            godot,
            &[path!("/work/godot_engine/modules/tcframework/editor/renderdoc_helper")],
            Expected::Unique,
            "directory",
        ),
        synthetic_entry(
            "已改的代码:Runtime/Core/DnEffectElement.cs，下一步看 WORKPLAN.md,里面写清了。",
            "Runtime/Core/DnEffectElement.cs",
            godot,
            &[path!("/work/godot_engine/Runtime/Core/DnEffectElement.cs")],
            Expected::Unique,
            "chinese_punctuation",
        ),
        synthetic_entry(
            "已改的代码:Runtime/Core/DnEffectElement.cs，下一步看 WORKPLAN.md,里面写清了。",
            "WORKPLAN.md",
            godot,
            &[path!("/work/godot_engine/WORKPLAN.md")],
            Expected::Unique,
            "file_name_only",
        ),
        synthetic_entry(
            "资源在 Reborn/解包/resource/uistring/uistring.xml 里。",
            "Reborn/解包/resource/uistring/uistring.xml",
            godot,
            &[path!("/work/godot_engine/Reborn/解包/resource/uistring/uistring.xml")],
            Expected::Unique,
            "chinese_folder",
        ),
        synthetic_entry(
            "两个目录 editor/plugins、editor/export 都要改。",
            "editor/export",
            godot,
            &[path!("/work/godot_engine/editor/export")],
            Expected::Unique,
            "directory",
        ),
        synthetic_entry(
            "- 这一段话很长，用来让终端在中间把路径折到下一行去看看会不会断开，路径是 scene/main/viewport_texture_helper.cpp:4410 这样写的。",
            "scene/main/viewport_texture_helper.cpp:4410",
            godot,
            &[path!("/work/godot_engine/scene/main/viewport_texture_helper.cpp")],
            Expected::Unique,
            "long_line",
        ),
        synthetic_entry(
            "UE 的 TArray 明确会变成空数组（Array.h:883）。",
            "Array.h:883",
            engine,
            &[
                path!("/work/Engine/Source/Runtime/Core/Public/Containers/Array.h"),
                path!("/work/Engine/Extras/ThirdPartyNotUE/libplist/include/plist/Array.h"),
                path!("/work/Engine/Source/ThirdParty/Eigen/Eigen/src/Core/Array.h"),
            ],
            Expected::Ambiguous,
            "same_name",
        ),
        synthetic_entry(
            "Unity 那边的 CSSpriteDamageLabel.cs:225 也是这样处理的。",
            "CSSpriteDamageLabel.cs:225",
            godot,
            &[path!("/work/UnityGame/Assets/UI/CSSpriteDamageLabel.cs")],
            Expected::InOtherProject,
            "other_project",
        ),
        // The other project, as a tab of its own.
        synthetic_entry(
            "入口在 Assets/UI/Main.cs。",
            "Assets/UI/Main.cs",
            path!("/work/UnityGame"),
            &[path!("/work/UnityGame/Assets/UI/Main.cs")],
            Expected::Unique,
            "file_name_only",
        ),
    ]
}

#[gpui::test]
async fn link_corpus_synthetic(cx: &mut TestAppContext) {
    let entries = synthetic_corpus();
    let (rates, failures) = run_corpus(cx, &entries).await;
    for (ix, columns, failure) in &failures {
        eprintln!(
            "{columns} columns: {:?} in {:?}: {failure:?}",
            entries[*ix].reference, entries[*ix].text
        );
    }
    assert!(failures.is_empty(), "{}", rates.report());
}

/// The real corpus, when `SOLO_LINK_CORPUS` names it.
#[gpui::test]
async fn link_corpus_from_env(cx: &mut TestAppContext) {
    let Some(path) = std::env::var_os("SOLO_LINK_CORPUS") else {
        eprintln!("SOLO_LINK_CORPUS isn't set: skipped");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("SOLO_LINK_CORPUS is readable");
    let mut skipped = 0;
    let entries = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let entry = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| CorpusEntry::parse(&value));
            if entry.is_none() {
                skipped += 1;
            }
            entry
        })
        .collect::<Vec<_>>();
    eprintln!("{} entries ({skipped} not understood)", entries.len());
    let (rates, failures) = run_corpus(cx, &entries).await;
    let report = json!({
        "rates": rates.report(),
        "failures": failures
            .iter()
            .map(|(ix, columns, failure)| json!({
                "entry": ix,
                "columns": columns,
                "reference": entries[*ix].reference,
                "failure": format!("{failure:?}"),
            }))
            .collect::<Vec<_>>(),
    });
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&report["rates"]).unwrap_or_default()
    );
    if let Some(report_path) = std::env::var_os("SOLO_LINK_CORPUS_REPORT") {
        std::fs::write(
            report_path,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        )
        .expect("the report is written");
    }
}

#[test]
fn renders_like_claude_code() {
    assert_eq!(
        render_like_claude_code("- see **`src/main.rs:12`** and more words here", 20),
        ["- see src/main.rs:12", "  and more words", "  here"]
    );
    // Chinese is two columns wide and breaks anywhere.
    assert_eq!(
        render_like_claude_code("中文中文中文中文中文中文", 10),
        ["中文中文中", "文中文中文", "中文"]
    );
    // A word too long for a line is broken in it.
    assert_eq!(
        render_like_claude_code("a/very/long/path/to/file.rs", 10),
        ["a/very/lon", "g/path/to/", "file.rs"]
    );
}

/// How long searching another tab of Unreal's size (about 260,000 files)
/// for a file name takes, and the same file name in this tab's own scan.
/// Run in release with `--ignored --nocapture`.
#[gpui::test]
#[ignore]
async fn bench_other_tab_search(cx: &mut TestAppContext) {
    use util::path;
    let fs = cx.update(AppState::test).fs.as_fake().clone();
    cx.update(|cx| {
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);
    });
    fs.insert_tree(path!("/godot"), json!({ "a.cpp": "" })).await;
    let mut tree = serde_json::Map::new();
    for folder in 0..2_600 {
        let files = (0..100)
            .map(|file| (format!("File_{file}.h"), json!("")))
            .collect::<serde_json::Map<_, _>>();
        tree.insert(format!("Module_{folder}"), Value::Object(files));
    }
    tree.insert(
        "Wanted".to_string(),
        json!({ "Array.h": "x\n".repeat(900) }),
    );
    fs.insert_tree(path!("/Engine"), Value::Object(tree)).await;
    let godot = Project::test(fs.clone(), [Path::new(path!("/godot"))], cx).await;
    let engine = Project::test(fs, [Path::new(path!("/Engine"))], cx).await;
    let (multi_workspace, cx) =
        cx.add_window_view(|window, cx| MultiWorkspace::test_new(godot.clone(), window, cx));
    let godot_workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().downgrade());
    let engine_workspace = multi_workspace.update_in(cx, |mw, window, cx| {
        mw.test_add_workspace(engine, window, cx)
    });
    cx.run_until_parked();
    for (label, workspace, working_directory) in [
        ("other tab", godot_workspace, path!("/godot")),
        ("own tab", engine_workspace.downgrade(), path!("/Engine")),
    ] {
        let target = PathLikeTarget {
            maybe_path: "Array.h:883".to_string(),
            working_directory: Some(PathBuf::from(working_directory)),
            context_paths: Vec::new(),
            position_files: Vec::new(),
        };
        let runs = 5;
        let started = std::time::Instant::now();
        for run in 0..runs {
            // A new name each run, so the remembered result isn't used.
            let target = PathLikeTarget {
                maybe_path: format!("Array.h:{}", 883 + run),
                ..target.clone()
            };
            let found = cx
                .update(|_, cx| {
                    resolve_path_like_target(
                        &workspace,
                        &target,
                        LinkPurpose::Click,
                        cx,
                        BackgroundPathChecks::LocalFileSystem,
                    )
                })
                .await;
            assert!(found.is_some());
        }
        eprintln!(
            "{label}: {:?} per search of 260,001 files",
            started.elapsed() / runs
        );
    }
}
