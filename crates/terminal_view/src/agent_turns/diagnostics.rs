//! SOLO: what a turn did to the language servers' diagnostics: the errors
//! and warnings it added in the files it changed, and the errors it fixed.
//!
//! When a turn starts, the diagnostics are counted per file, and those of
//! open files kept in full. When it ends, the files it changed are opened (so
//! servers that only check open files, like clangd, check them), and once
//! the servers are done their diagnostics are compared with those from the
//! start. Files that weren't open at the start only have counts to compare.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use collections::HashMap;
use gpui::{App, AsyncApp, Entity, TaskExt as _, WeakEntity};
use language::{Buffer, DiagnosticSeverity, Point};
use project::Project;

/// How long the language servers get to check the changed files (with
/// rust-analyzer, a `cargo check`).
const MAX_WAIT: Duration = Duration::from_secs(60);
/// At least this long, for servers to see the files and start.
const MIN_WAIT: Duration = Duration::from_secs(2);
/// Diagnostics that haven't changed for this long, with no server busy, are
/// taken as final.
const SETTLED_AFTER: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(500);
/// Changed files checked per turn, at most.
const MAX_FILES: usize = 50;

/// An error or warning, as compared between the start and the end of a turn.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DiagnosticKey {
    is_error: bool,
    message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnDiagnostic {
    pub path: PathBuf,
    /// From 1.
    pub line: u32,
    pub column: u32,
    pub is_error: bool,
    pub message: String,
    pub source: Option<String>,
}

impl TurnDiagnostic {
    fn key(&self) -> DiagnosticKey {
        DiagnosticKey {
            is_error: self.is_error,
            message: self.message.clone(),
        }
    }
}

/// The diagnostics when a turn started.
#[derive(Clone, Debug, Default)]
pub(crate) struct DiagnosticsBefore {
    /// Errors and warnings per file, from the language servers' summaries.
    counts: HashMap<PathBuf, (usize, usize)>,
    /// The diagnostics of the files open then.
    open_files: HashMap<PathBuf, Vec<DiagnosticKey>>,
}

/// What a turn did to the diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnDiagnostics {
    /// The language servers are still checking the changed files.
    Checking,
    /// No language server checked them, or not in time.
    Unchecked,
    Checked {
        /// New errors and warnings, errors first.
        added: Vec<TurnDiagnostic>,
        /// New ones in files that weren't open when the turn started, which
        /// can only be counted, not told apart from the old ones.
        added_uncertain: usize,
        fixed_errors: usize,
    },
}

impl TurnDiagnostics {
    /// New errors, and errors fixed, when checked.
    pub(crate) fn error_delta(&self) -> Option<(usize, usize)> {
        match self {
            TurnDiagnostics::Checked {
                added,
                added_uncertain,
                fixed_errors,
            } => Some((
                added
                    .iter()
                    .filter(|diagnostic| diagnostic.is_error)
                    .count()
                    + added_uncertain,
                *fixed_errors,
            )),
            _ => None,
        }
    }
}

fn buffer_diagnostics(buffer: &Buffer) -> Vec<(Point, DiagnosticKey, Option<String>)> {
    let snapshot = buffer.snapshot();
    snapshot
        .diagnostics_in_range::<_, Point>(0..snapshot.len(), false)
        .filter(|entry| entry.diagnostic.is_primary)
        .filter_map(|entry| {
            let is_error = match entry.diagnostic.severity {
                DiagnosticSeverity::ERROR => true,
                DiagnosticSeverity::WARNING => false,
                _ => return None,
            };
            Some((
                entry.range.start,
                DiagnosticKey {
                    is_error,
                    message: entry.diagnostic.message.to_string(),
                },
                entry.diagnostic.source.clone(),
            ))
        })
        .collect()
}

/// Records the diagnostics as a turn starts.
pub(crate) fn capture_before(project: &Entity<Project>, cx: &App) -> DiagnosticsBefore {
    let project = project.read(cx);
    let mut counts: HashMap<PathBuf, (usize, usize)> = HashMap::default();
    for (project_path, _, summary) in project.diagnostic_summaries(false, cx) {
        if let Some(path) = project.absolute_path(&project_path, cx) {
            let count = counts.entry(path).or_default();
            count.0 += summary.error_count;
            count.1 += summary.warning_count;
        }
    }
    let mut open_files = HashMap::default();
    for buffer in project.buffer_store().read(cx).buffers() {
        let buffer = buffer.read(cx);
        let Some(path) = buffer
            .file()
            .and_then(|file| file.as_local())
            .map(|file| file.abs_path(cx))
        else {
            continue;
        };
        let diagnostics = buffer_diagnostics(buffer)
            .into_iter()
            .map(|(_, key, _)| key)
            .collect();
        open_files.insert(path, diagnostics);
    }
    DiagnosticsBefore { counts, open_files }
}

/// Compares the diagnostics of the changed files with those from before.
fn compare(
    before: &DiagnosticsBefore,
    after: &[(PathBuf, Vec<TurnDiagnostic>)],
) -> TurnDiagnostics {
    let mut added = Vec::new();
    let mut added_uncertain = 0;
    let mut fixed_errors = 0;
    for (path, diagnostics) in after {
        match before.open_files.get(path) {
            Some(previous) => {
                let mut remaining: Vec<&DiagnosticKey> = previous.iter().collect();
                for diagnostic in diagnostics {
                    let key = diagnostic.key();
                    match remaining.iter().position(|previous| **previous == key) {
                        Some(index) => {
                            remaining.swap_remove(index);
                        }
                        None => added.push(diagnostic.clone()),
                    }
                }
                fixed_errors += remaining.iter().filter(|key| key.is_error).count();
            }
            None => {
                let (errors_before, warnings_before) =
                    before.counts.get(path).copied().unwrap_or_default();
                let errors = diagnostics.iter().filter(|diagnostic| diagnostic.is_error);
                let warnings = diagnostics.iter().filter(|diagnostic| !diagnostic.is_error);
                let error_count = errors.clone().count();
                // With nothing before, everything is new.
                if errors_before == 0 {
                    added.extend(errors.cloned());
                } else {
                    added_uncertain += error_count.saturating_sub(errors_before);
                }
                // New warnings among old ones aren't counted: they matter less.
                if warnings_before == 0 {
                    added.extend(warnings.cloned());
                }
                fixed_errors += errors_before.saturating_sub(error_count);
            }
        }
    }
    added.sort_by(|a, b| {
        (!a.is_error, &a.path, a.line, a.column).cmp(&(!b.is_error, &b.path, b.line, b.column))
    });
    TurnDiagnostics::Checked {
        added,
        added_uncertain,
        fixed_errors,
    }
}

/// A fingerprint of the diagnostics in `buffers`, to tell when they settle.
fn fingerprint(buffers: &[Entity<Buffer>], cx: &App) -> Vec<usize> {
    buffers
        .iter()
        .map(|buffer| buffer_diagnostics(buffer.read(cx)).len())
        .collect()
}

/// Whether the language servers have the changed files: each file is open in
/// a server, or has no server for its language, and at least one is open in
/// one.
fn servers_have_the_files(
    project: &Entity<Project>,
    buffers: &[Entity<Buffer>],
    cx: &mut App,
) -> bool {
    let lsp_store = project.read(cx).lsp_store();
    let mut any_open = false;
    for buffer in buffers {
        let buffer_id = buffer.read(cx).remote_id();
        let open = lsp_store
            .read(cx)
            .language_server_ids_for_opened_buffer(buffer_id)
            .is_some_and(|servers| !servers.is_empty());
        if open {
            any_open = true;
            continue;
        }
        let expected = lsp_store.update(cx, |lsp_store, cx| {
            buffer.update(cx, |buffer, cx| {
                !lsp_store
                    .language_servers_for_local_buffer(buffer, cx)
                    .is_empty()
            })
        });
        if expected {
            return false;
        }
    }
    any_open
}

/// Checks the files a turn changed once the language servers are done with
/// them, and compares them with `before`.
pub(crate) async fn check_turn(
    project: WeakEntity<Project>,
    before: DiagnosticsBefore,
    changed: Vec<PathBuf>,
    cx: &mut AsyncApp,
) -> TurnDiagnostics {
    let Some(project) = project.upgrade() else {
        return TurnDiagnostics::Unchecked;
    };
    let has_servers = cx.update(|cx| {
        project
            .read(cx)
            .language_server_statuses(cx)
            .next()
            .is_some()
    });
    if !has_servers || changed.is_empty() {
        return TurnDiagnostics::Unchecked;
    }
    let mut buffers = Vec::new();
    for path in changed.into_iter().take(MAX_FILES) {
        let opened = project
            .update(cx, |project, cx| project.open_local_buffer(&path, cx))
            .await;
        if let Ok(buffer) = opened {
            buffers.push((path, buffer));
        }
    }
    if buffers.is_empty() {
        return TurnDiagnostics::Unchecked;
    }
    let entities: Vec<Entity<Buffer>> = buffers.iter().map(|(_, buffer)| buffer.clone()).collect();

    let started = Instant::now();
    let mut last = cx.update(|cx| fingerprint(&entities, cx));
    let mut unchanged_since = started;
    let mut checked_on_disk = false;
    loop {
        cx.background_executor().timer(POLL).await;
        let (now_fingerprint, busy, attached) = cx.update(|cx| {
            let busy = project
                .read(cx)
                .language_server_statuses(cx)
                .any(|(_, status)| {
                    !status.pending_work.is_empty() || status.has_pending_diagnostic_updates
                });
            (
                fingerprint(&entities, cx),
                busy,
                servers_have_the_files(&project, &entities, cx),
            )
        });
        if now_fingerprint != last {
            last = now_fingerprint;
            unchanged_since = Instant::now();
        }
        // Until the servers have the files (one may still be starting, or
        // downloading), no diagnostics only means none came yet.
        if !attached {
            unchanged_since = Instant::now();
        } else if !checked_on_disk {
            // rust-analyzer reports most errors from `cargo check`, which it
            // runs when a file is saved in the editor; the agent wrote the
            // files itself. Without rust-analyzer, this does nothing.
            checked_on_disk = true;
            unchanged_since = Instant::now();
            cx.update(|cx| {
                project::lsp_store::rust_analyzer_ext::run_flycheck(project.clone(), None, cx)
                    .detach_and_log_err(cx)
            });
        }
        let elapsed = started.elapsed();
        if elapsed >= MIN_WAIT && !busy && unchanged_since.elapsed() >= SETTLED_AFTER {
            break;
        }
        if elapsed >= MAX_WAIT {
            return TurnDiagnostics::Unchecked;
        }
    }

    let after: Vec<(PathBuf, Vec<TurnDiagnostic>)> = cx.update(|cx| {
        buffers
            .iter()
            .map(|(path, buffer)| {
                let diagnostics = buffer_diagnostics(buffer.read(cx))
                    .into_iter()
                    .map(|(point, key, source)| TurnDiagnostic {
                        path: path.clone(),
                        line: point.row + 1,
                        column: point.column + 1,
                        is_error: key.is_error,
                        message: key.message,
                        source,
                    })
                    .collect();
                (path.clone(), diagnostics)
            })
            .collect()
    });
    compare(&before, &after)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(path: &str, line: u32, is_error: bool, message: &str) -> TurnDiagnostic {
        TurnDiagnostic {
            path: PathBuf::from(path),
            line,
            column: 1,
            is_error,
            message: message.to_string(),
            source: None,
        }
    }

    fn key(is_error: bool, message: &str) -> DiagnosticKey {
        DiagnosticKey {
            is_error,
            message: message.to_string(),
        }
    }

    #[test]
    fn compares_open_files_by_message() {
        let mut before = DiagnosticsBefore::default();
        before.open_files.insert(
            PathBuf::from("a.rs"),
            vec![
                key(true, "old error"),
                key(true, "fixed error"),
                key(false, "old warning"),
            ],
        );
        let after = vec![(
            PathBuf::from("a.rs"),
            vec![
                // Moved by the edit, still the same.
                diagnostic("a.rs", 40, true, "old error"),
                diagnostic("a.rs", 41, false, "old warning"),
                diagnostic("a.rs", 7, true, "new error"),
                diagnostic("a.rs", 3, false, "new warning"),
            ],
        )];
        let result = compare(&before, &after);
        assert_eq!(result.error_delta(), Some((1, 1)));
        let TurnDiagnostics::Checked { added, .. } = result else {
            panic!("not checked");
        };
        assert_eq!(
            added
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec!["new error", "new warning"]
        );
    }

    #[test]
    fn counts_files_that_were_not_open() {
        let mut before = DiagnosticsBefore::default();
        before.counts.insert(PathBuf::from("b.rs"), (2, 0));
        let after = vec![
            // Clean before: everything in it is new.
            (
                PathBuf::from("clean.rs"),
                vec![diagnostic("clean.rs", 1, true, "e")],
            ),
            // Two errors before, three now: one more, which one isn't known.
            (
                PathBuf::from("b.rs"),
                vec![
                    diagnostic("b.rs", 1, true, "x"),
                    diagnostic("b.rs", 2, true, "y"),
                    diagnostic("b.rs", 3, true, "z"),
                ],
            ),
        ];
        let result = compare(&before, &after);
        assert_eq!(result.error_delta(), Some((2, 0)));
        let TurnDiagnostics::Checked {
            added,
            added_uncertain,
            ..
        } = result
        else {
            panic!("not checked");
        };
        assert_eq!(added.len(), 1);
        assert_eq!(added_uncertain, 1);

        // Fewer errors than before: fixed.
        let after = vec![(PathBuf::from("b.rs"), vec![])];
        assert_eq!(compare(&before, &after).error_delta(), Some((0, 2)));
    }

    #[test]
    fn unchecked_turns_have_no_counts() {
        assert_eq!(TurnDiagnostics::Unchecked.error_delta(), None);
        assert_eq!(TurnDiagnostics::Checking.error_delta(), None);
    }
}
