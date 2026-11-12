use super::{HoverTarget, HoveredWord, TerminalView};
use anyhow::Result;
use editor::items::open_resolved_target;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Task, TaskExt, WeakEntity, Window};
use project::Project;
use std::path::PathBuf;
use terminal::PathLikeTarget;
use workspace::path_link::PathMatching;
#[cfg(not(test))]
use workspace::path_link::resolve_open_target_near;
#[cfg(test)]
use workspace::path_link::{
    BackgroundPathChecks, OpenTargetFoundBy, resolve_open_target_with_fs_checks,
};
use workspace::{Workspace, path_link::OpenTarget};

pub(super) fn hover_path_like_target(
    workspace: &WeakEntity<Workspace>,
    hovered_word: HoveredWord,
    path_like_target: &PathLikeTarget,
    cx: &mut Context<TerminalView>,
) -> Task<()> {
    #[cfg(not(test))]
    {
        possible_hover_target(workspace, hovered_word, path_like_target, cx)
    }
    #[cfg(test)]
    {
        possible_hover_target(
            workspace,
            hovered_word,
            path_like_target,
            cx,
            BackgroundPathChecks::LocalFileSystem,
        )
    }
}

fn possible_hover_target(
    workspace: &WeakEntity<Workspace>,
    hovered_word: HoveredWord,
    path_like_target: &PathLikeTarget,
    cx: &mut Context<TerminalView>,
    #[cfg(test)] background_path_checks: BackgroundPathChecks,
) -> Task<()> {
    let file_to_open_task = resolve_path_like_target(
        workspace,
        path_like_target,
        cx,
        #[cfg(test)]
        background_path_checks,
    );
    cx.spawn(async move |terminal_view, cx| {
        let file_to_open = file_to_open_task.await;
        terminal_view
            .update(cx, |terminal_view, cx| {
                match file_to_open {
                    Some(OpenTarget::Path(path, ..) | OpenTarget::Worktree(path, ..)) => {
                        terminal_view.hover = Some(HoverTarget {
                            tooltip: path
                                .to_string(&|path: &PathBuf| path.to_string_lossy().into_owned()),
                            hovered_word,
                        });
                    }
                    None => {
                        terminal_view.hover = None;
                    }
                };
                cx.notify();
            })
            .ok();
    })
}

pub(super) fn open_path_like_target(
    workspace: &WeakEntity<Workspace>,
    terminal_view: &mut TerminalView,
    path_like_target: &PathLikeTarget,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) {
    #[cfg(not(test))]
    {
        possibly_open_target(workspace, terminal_view, path_like_target, window, cx)
            .detach_and_log_err(cx)
    }
    #[cfg(test)]
    {
        possibly_open_target(
            workspace,
            terminal_view,
            path_like_target,
            window,
            cx,
            BackgroundPathChecks::LocalFileSystem,
        )
        .detach_and_log_err(cx)
    }
}

fn possibly_open_target(
    workspace: &WeakEntity<Workspace>,
    terminal_view: &mut TerminalView,
    path_like_target: &PathLikeTarget,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
    #[cfg(test)] background_path_checks: BackgroundPathChecks,
) -> Task<Result<Option<OpenTarget>>> {
    if terminal_view.hover.is_none() {
        return Task::ready(Ok(None));
    }
    let workspace = workspace.clone();
    let path_like_target = path_like_target.clone();
    cx.spawn_in(window, async move |terminal_view, cx| {
        let Some(open_target) = terminal_view
            .update(cx, |_, cx| {
                resolve_path_like_target(
                    &workspace,
                    &path_like_target,
                    cx,
                    #[cfg(test)]
                    background_path_checks,
                )
            })?
            .await
        else {
            return Ok(None);
        };

        let opened = open_resolved_target(&workspace, &open_target, cx).await?;
        Ok(opened.then_some(open_target))
    })
}

/// SOLO: what a path link opens, for its tooltip and its click alike. A bare
/// position (`:3788`) put in the file named nearest before it is tried in
/// the other files named before it too, nearest first, and goes to the first
/// that exists and has that line; when none does, to the nearest that exists.
fn resolve_path_like_target(
    workspace: &WeakEntity<Workspace>,
    path_like_target: &PathLikeTarget,
    cx: &App,
    #[cfg(test)] background_path_checks: BackgroundPathChecks,
) -> Task<Option<OpenTarget>> {
    let resolve = |maybe_path: &str| {
        #[cfg(not(test))]
        {
            resolve_open_target_near(
                workspace,
                PathMatching::Heuristic,
                maybe_path,
                path_like_target.working_directory.as_deref(),
                &path_like_target.context_paths,
                cx,
            )
        }
        #[cfg(test)]
        {
            resolve_open_target_with_fs_checks(
                workspace,
                PathMatching::Heuristic,
                maybe_path,
                path_like_target.working_directory.as_deref(),
                &path_like_target.context_paths,
                cx,
                background_path_checks,
            )
        }
    };
    let nearest = resolve(&path_like_target.maybe_path);
    if path_like_target.position_files.is_empty() {
        return nearest;
    }
    let Some(project) = workspace
        .upgrade()
        .map(|workspace| workspace.read(cx).project().clone())
    else {
        return nearest;
    };
    // All are resolved at once, so hovering waits for the slowest one, not
    // for all of them in turn.
    let candidates = std::iter::once(nearest)
        .chain(
            path_like_target
                .position_files
                .iter()
                .map(|maybe_path| resolve(maybe_path)),
        )
        .collect::<Vec<_>>();
    cx.spawn(async move |cx| {
        let candidates = futures::future::join_all(candidates).await;
        for candidate in candidates.iter().flatten() {
            if has_row(candidate, &project, cx).await {
                return Some(candidate.clone());
            }
        }
        // None has the line: the nearest that exists, rather than no link
        // when the nearest isn't a file at all (e.g. `classes.getptr`).
        candidates.into_iter().flatten().next()
    })
}

/// SOLO: whether the file `target` is has the line it names: counted in its
/// buffer when it's open, else read from disk up to that line, in the
/// background. A remote file, or one that can't be read, is taken to have it.
async fn has_row(target: &OpenTarget, project: &Entity<Project>, cx: &mut AsyncApp) -> bool {
    if !target.is_file() {
        return false;
    }
    let Some(row) = target.path().row.filter(|row| *row > 1) else {
        return true;
    };
    let path = target.path().path.clone();
    let (open_buffer_lines, fs) = project.read_with(cx, |project, cx| {
        let open_buffer_lines = project
            .find_project_path(&path, cx)
            .and_then(|project_path| project.get_open_buffer(&project_path, cx))
            .map(|buffer| {
                let max_point = buffer.read(cx).max_point();
                // A trailing newline doesn't start another line.
                if max_point.column == 0 && max_point.row > 0 {
                    max_point.row
                } else {
                    max_point.row + 1
                }
            });
        (
            open_buffer_lines,
            project.is_local().then(|| project.fs().clone()),
        )
    });
    if let Some(lines) = open_buffer_lines {
        return lines >= row;
    }
    let Some(fs) = fs else {
        return true;
    };
    cx.background_spawn(async move {
        match fs.open_sync(&path).await {
            Ok(file) => has_line(file, row).unwrap_or(true),
            Err(_) => true,
        }
    })
    .await
}

/// SOLO: whether `file` has a line `row` (1-based), reading only as far as it.
fn has_line(mut file: impl std::io::Read, row: u32) -> std::io::Result<bool> {
    let mut buffer = [0; 64 * 1024];
    let mut newlines = 0;
    let mut ends_in_newline = true;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            if *byte == b'\n' {
                newlines += 1;
                if newlines >= row {
                    return Ok(true);
                }
            }
        }
        ends_in_newline = buffer[read - 1] == b'\n';
    }
    Ok(newlines + u32::from(!ends_in_newline) >= row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::Project;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use terminal::{
        HoveredWord, Point, Range, TerminalBuilder,
        terminal_settings::{AlternateScroll, CursorShape},
    };
    use util::path;
    use util::paths::PathStyle;
    use workspace::{AppState, MultiWorkspace};

    async fn init_test(
        app_cx: &mut TestAppContext,
        trees: impl IntoIterator<Item = (&str, serde_json::Value)>,
        worktree_roots: impl IntoIterator<Item = &str>,
    ) -> impl AsyncFnMut(
        HoveredWord,
        PathLikeTarget,
        BackgroundPathChecks,
    ) -> (Option<HoverTarget>, Option<OpenTarget>) {
        let fs = app_cx.update(AppState::test).fs.as_fake().clone();

        app_cx.update(|cx| {
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });

        for (path, tree) in trees {
            fs.insert_tree(path, tree).await;
        }

        let project: gpui::Entity<Project> = Project::test(
            fs.clone(),
            worktree_roots.into_iter().map(Path::new),
            app_cx,
        )
        .await;

        let (multi_workspace, cx) = app_cx
            .add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());

        let terminal = app_cx.new(|cx| {
            TerminalBuilder::new_display_only(
                CursorShape::default(),
                AlternateScroll::On,
                None,
                0,
                cx.background_executor(),
                PathStyle::local(),
            )
            .subscribe(cx)
        });

        let workspace_a = workspace.clone();
        let (terminal_view, cx) = app_cx.add_window_view(|window, cx| {
            TerminalView::new(
                terminal,
                workspace_a.downgrade(),
                None,
                project.downgrade(),
                window,
                cx,
            )
        });

        async move |hovered_word: HoveredWord,
                    path_like_target: PathLikeTarget,
                    background_path_checks: BackgroundPathChecks|
                    -> (Option<HoverTarget>, Option<OpenTarget>) {
            let workspace_a = workspace.clone();
            terminal_view
                .update(cx, |_, cx| {
                    possible_hover_target(
                        &workspace_a.downgrade(),
                        hovered_word,
                        &path_like_target,
                        cx,
                        background_path_checks,
                    )
                })
                .await;

            let hover_target =
                terminal_view.read_with(cx, |terminal_view, _| terminal_view.hover.clone());

            let open_target = terminal_view
                .update_in(cx, |terminal_view, window, cx| {
                    possibly_open_target(
                        &workspace.downgrade(),
                        terminal_view,
                        &path_like_target,
                        window,
                        cx,
                        background_path_checks,
                    )
                })
                .await
                .expect("Failed to possibly open target");

            (hover_target, open_target)
        }
    }

    async fn test_path_like_simple(
        test_path_like: &mut impl AsyncFnMut(
            HoveredWord,
            PathLikeTarget,
            BackgroundPathChecks,
        ) -> (Option<HoverTarget>, Option<OpenTarget>),
        maybe_path: &str,
        tooltip: &str,
        working_directory: Option<PathBuf>,
        background_path_checks: BackgroundPathChecks,
        open_target_found_by: OpenTargetFoundBy,
        file: &str,
        line: u32,
    ) {
        let (hover_target, open_target) = test_path_like(
            HoveredWord {
                word: maybe_path.to_string(),
                word_match: Range::new(Point::new(0, 0), Point::new(0, 0)),
                id: 0,
            },
            PathLikeTarget {
                maybe_path: maybe_path.to_string(),
                working_directory,
                context_paths: Vec::new(),
                position_files: Vec::new(),
            },
            background_path_checks,
        )
        .await;

        let Some(hover_target) = hover_target else {
            assert!(
                hover_target.is_some(),
                "Hover target should not be `None` at {file}:{line}:"
            );
            return;
        };

        assert_eq!(
            hover_target.tooltip, tooltip,
            "Tooltip mismatch at {file}:{line}:"
        );
        assert_eq!(
            hover_target.hovered_word.word, maybe_path,
            "Hovered word mismatch at {file}:{line}:"
        );

        let Some(open_target) = open_target else {
            assert!(
                open_target.is_some(),
                "Open target should not be `None` at {file}:{line}:"
            );
            return;
        };

        assert_eq!(
            open_target.path().path,
            Path::new(tooltip),
            "Open target path mismatch at {file}:{line}:"
        );

        assert_eq!(
            open_target.found_by(),
            open_target_found_by,
            "Open target found by mismatch at {file}:{line}:"
        );
    }

    macro_rules! none_or_some_pathbuf {
        (None) => {
            None
        };
        ($cwd:literal) => {
            Some($crate::PathBuf::from(path!($cwd)))
        };
    }

    macro_rules! test_path_like {
        (
            $test_path_like:expr,
            $maybe_path:literal,
            $tooltip:literal,
            $cwd:tt,
            $found_by:expr
        ) => {{
            test_path_like!(
                $test_path_like,
                $maybe_path,
                $tooltip,
                $cwd,
                BackgroundPathChecks::LocalFileSystem,
                $found_by
            );
            test_path_like!(
                $test_path_like,
                $maybe_path,
                $tooltip,
                $cwd,
                BackgroundPathChecks::ProjectPathResolution,
                $found_by
            );
        }};

        (
            $test_path_like:expr,
            $maybe_path:literal,
            $tooltip:literal,
            $cwd:tt,
            $background_fs_checks:path,
            $found_by:expr
        ) => {
            test_path_like_simple(
                &mut $test_path_like,
                path!($maybe_path),
                path!($tooltip),
                none_or_some_pathbuf!($cwd),
                $background_fs_checks,
                $found_by,
                std::file!(),
                std::line!(),
            )
            .await
        };
    }

    // Note the arms of `test`, `test_local`, and `test_remote` should be collapsed once macro
    // metavariable expressions (#![feature(macro_metavar_expr)]) are stabilized.
    // See https://github.com/rust-lang/rust/issues/83527
    #[doc = "test_path_likes!(<cx>, <trees>, <worktrees>, { $(<tests>;)+ })"]
    macro_rules! test_path_likes {
        ($cx:expr, $trees:expr, $worktrees:expr, { $($tests:expr;)+ }) => { {
            let mut test_path_like = init_test($cx, $trees, $worktrees).await;
            #[doc ="test!(<hovered maybe_path>, <expected tooltip>, <terminal cwd> "]
            #[doc ="\\[, found by \\])"]
            #[allow(unused_macros)]
            macro_rules! test {
                ($maybe_path:literal, $tooltip:literal, $cwd:tt) => {
                    test_path_like!(
                        test_path_like,
                        $maybe_path,
                        $tooltip,
                        $cwd,
                        OpenTargetFoundBy::WorktreeExact
                    )
                };
                ($maybe_path:literal, $tooltip:literal, $cwd:tt, $found_by:ident) => {
                    test_path_like!(
                        test_path_like,
                        $maybe_path,
                        $tooltip,
                        $cwd,
                        OpenTargetFoundBy::$found_by
                    )
                }
            }
            #[doc ="test_local!(<hovered maybe_path>, <expected tooltip>, <terminal cwd> "]
            #[doc ="\\[, found by \\])"]
            #[allow(unused_macros)]
            macro_rules! test_local {
                ($maybe_path:literal, $tooltip:literal, $cwd:tt) => {
                    test_path_like!(
                        test_path_like,
                        $maybe_path,
                        $tooltip,
                        $cwd,
                        BackgroundPathChecks::LocalFileSystem,
                        OpenTargetFoundBy::WorktreeExact
                    )
                };
                ($maybe_path:literal, $tooltip:literal, $cwd:tt, $found_by:ident) => {
                    test_path_like!(
                        test_path_like,
                        $maybe_path,
                        $tooltip,
                        $cwd,
                        BackgroundPathChecks::LocalFileSystem,
                        OpenTargetFoundBy::$found_by
                    )
                }
            }
            #[doc ="test_remote!(<hovered maybe_path>, <expected tooltip>, <terminal cwd> "]
            #[doc ="\\[, found by \\])"]
            #[allow(unused_macros)]
            macro_rules! test_remote {
                ($maybe_path:literal, $tooltip:literal, $cwd:tt) => {
                    test_path_like!(
                        test_path_like,
                        $maybe_path,
                        $tooltip,
                        $cwd,
                        BackgroundPathChecks::ProjectPathResolution,
                        OpenTargetFoundBy::WorktreeExact
                    )
                };
                ($maybe_path:literal, $tooltip:literal, $cwd:tt, $found_by:ident) => {
                    test_path_like!(
                        test_path_like,
                        $maybe_path,
                        $tooltip,
                        $cwd,
                        BackgroundPathChecks::ProjectPathResolution,
                        OpenTargetFoundBy::$found_by
                    )
                }
            }
            $($tests);+
        } }
    }

    #[gpui::test]
    async fn one_folder_worktree(cx: &mut TestAppContext) {
        test_path_likes!(
            cx,
            vec![(
                path!("/test"),
                json!({
                    "lib.rs": "",
                    "test.rs": "",
                }),
            )],
            vec![path!("/test")],
            {
                test!("lib.rs", "/test/lib.rs", None);
                test!("/test/lib.rs", "/test/lib.rs", None);
                test!("test.rs", "/test/test.rs", None);
                test!("/test/test.rs", "/test/test.rs", None);
            }
        )
    }

    #[gpui::test]
    async fn mixed_worktrees(cx: &mut TestAppContext) {
        test_path_likes!(
            cx,
            vec![
                (
                    path!("/"),
                    json!({
                        "file.txt": "",
                    }),
                ),
                (
                    path!("/test"),
                    json!({
                        "lib.rs": "",
                        "test.rs": "",
                        "file.txt": "",
                    }),
                ),
            ],
            vec![path!("/file.txt"), path!("/test")],
            {
                test!("file.txt", "/file.txt", "/");
                test!("/file.txt", "/file.txt", "/");

                test!("lib.rs", "/test/lib.rs", "/test");
                test!("test.rs", "/test/test.rs", "/test");
                test!("file.txt", "/test/file.txt", "/test");

                test!("/test/lib.rs", "/test/lib.rs", "/test");
                test!("/test/test.rs", "/test/test.rs", "/test");
                test!("/test/file.txt", "/test/file.txt", "/test");
            }
        )
    }

    #[gpui::test]
    async fn worktree_file_preferred(cx: &mut TestAppContext) {
        test_path_likes!(
            cx,
            vec![
                (
                    path!("/"),
                    json!({
                        "file.txt": "",
                    }),
                ),
                (
                    path!("/test"),
                    json!({
                        "file.txt": "",
                    }),
                ),
            ],
            vec![path!("/test")],
            {
                test!("file.txt", "/test/file.txt", "/test");
            }
        )
    }

    mod issues {
        use super::*;

        // https://github.com/zed-industries/zed/issues/28407
        #[gpui::test]
        async fn issue_28407_siblings(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/dir1"),
                    json!({
                        "dir 2": {
                            "C.py": ""
                        },
                        "dir 3": {
                            "C.py": ""
                        },
                    }),
                )],
                vec![path!("/dir1")],
                {
                    test!("C.py", "/dir1/dir 2/C.py", "/dir1", WorktreeScan);
                    test!("C.py", "/dir1/dir 2/C.py", "/dir1/dir 2");
                    test!("C.py", "/dir1/dir 3/C.py", "/dir1/dir 3");
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/28407
        // See https://github.com/zed-industries/zed/issues/34027
        // See https://github.com/zed-industries/zed/issues/33498
        #[gpui::test]
        async fn issue_28407_nesting(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/project"),
                    json!({
                        "lib": {
                            "src": {
                                "main.rs": "",
                                "only_in_lib.rs": ""
                            },
                        },
                        "src": {
                            "main.rs": ""
                        },
                    }),
                )],
                vec![path!("/project")],
                {
                    test!("main.rs", "/project/src/main.rs", "/project/src");
                    test!("main.rs", "/project/lib/src/main.rs", "/project/lib/src");

                    test!("src/main.rs", "/project/src/main.rs", "/project");
                    test!("src/main.rs", "/project/src/main.rs", "/project/src");
                    test!("src/main.rs", "/project/lib/src/main.rs", "/project/lib");

                    test!("lib/src/main.rs", "/project/lib/src/main.rs", "/project");
                    test!(
                        "lib/src/main.rs",
                        "/project/lib/src/main.rs",
                        "/project/src"
                    );
                    test!(
                        "lib/src/main.rs",
                        "/project/lib/src/main.rs",
                        "/project/lib"
                    );
                    test!(
                        "lib/src/main.rs",
                        "/project/lib/src/main.rs",
                        "/project/lib/src"
                    );
                    test!(
                        "src/only_in_lib.rs",
                        "/project/lib/src/only_in_lib.rs",
                        "/project/lib/src",
                        WorktreeScan
                    );
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/28339
        #[gpui::test]
        async fn issue_28339(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/tmp"),
                    json!({
                        "issue28339": {
                            "foo": {
                                "bar.txt": ""
                            },
                        },
                    }),
                )],
                vec![path!("/tmp")],
                {
                    test_local!(
                        "foo/./bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339",
                        WorktreeExact
                    );
                    test_local!(
                        "foo/../foo/bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339",
                        WorktreeExact
                    );
                    test_local!(
                        "foo/..///foo/bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339",
                        WorktreeExact
                    );
                    test_local!(
                        "issue28339/../issue28339/foo/../foo/bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339",
                        WorktreeExact
                    );
                    test_local!(
                        "./bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339/foo",
                        WorktreeExact
                    );
                    test_local!(
                        "../foo/bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339/foo",
                        WorktreeExact
                    );
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/28339
        #[gpui::test]
        async fn issue_28339_remote(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/tmp"),
                    json!({
                        "issue28339": {
                            "foo": {
                                "bar.txt": ""
                            },
                        },
                    }),
                )],
                vec![path!("/tmp")],
                {
                    test_remote!(
                        "foo/./bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339"
                    );
                    test_remote!(
                        "foo/../foo/bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339"
                    );
                    test_remote!(
                        "foo/..///foo/bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339"
                    );
                    test_remote!(
                        "issue28339/../issue28339/foo/../foo/bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339"
                    );
                    test_remote!(
                        "./bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339/foo"
                    );
                    test_remote!(
                        "../foo/bar.txt",
                        "/tmp/issue28339/foo/bar.txt",
                        "/tmp/issue28339/foo"
                    );
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/34027
        #[gpui::test]
        async fn issue_34027(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/tmp/issue34027"),
                    json!({
                        "test.txt": "",
                        "foo": {
                            "test.txt": "",
                        }
                    }),
                ),],
                vec![path!("/tmp/issue34027")],
                {
                    test!("test.txt", "/tmp/issue34027/test.txt", "/tmp/issue34027");
                    test!(
                        "test.txt",
                        "/tmp/issue34027/foo/test.txt",
                        "/tmp/issue34027/foo"
                    );
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/34027
        #[gpui::test]
        async fn issue_34027_siblings(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/test"),
                    json!({
                        "sub1": {
                            "file.txt": "",
                        },
                        "sub2": {
                            "file.txt": "",
                        }
                    }),
                ),],
                vec![path!("/test")],
                {
                    test!("file.txt", "/test/sub1/file.txt", "/test/sub1");
                    test!("file.txt", "/test/sub2/file.txt", "/test/sub2");
                    test!("sub1/file.txt", "/test/sub1/file.txt", "/test/sub1");
                    test!("sub2/file.txt", "/test/sub2/file.txt", "/test/sub2");
                    test!("sub1/file.txt", "/test/sub1/file.txt", "/test/sub2");
                    test!("sub2/file.txt", "/test/sub2/file.txt", "/test/sub1");
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/34027
        #[gpui::test]
        async fn issue_34027_nesting(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/test"),
                    json!({
                        "sub1": {
                            "file.txt": "",
                            "subsub1": {
                                "file.txt": "",
                            }
                        },
                        "sub2": {
                            "file.txt": "",
                            "subsub1": {
                                "file.txt": "",
                            }
                        }
                    }),
                ),],
                vec![path!("/test")],
                {
                    test!(
                        "file.txt",
                        "/test/sub1/subsub1/file.txt",
                        "/test/sub1/subsub1"
                    );
                    test!(
                        "file.txt",
                        "/test/sub2/subsub1/file.txt",
                        "/test/sub2/subsub1"
                    );
                    test!(
                        "subsub1/file.txt",
                        "/test/sub1/subsub1/file.txt",
                        "/test",
                        WorktreeScan
                    );
                    test!(
                        "subsub1/file.txt",
                        "/test/sub1/subsub1/file.txt",
                        "/test",
                        WorktreeScan
                    );
                    test!(
                        "subsub1/file.txt",
                        "/test/sub1/subsub1/file.txt",
                        "/test/sub1"
                    );
                    test!(
                        "subsub1/file.txt",
                        "/test/sub2/subsub1/file.txt",
                        "/test/sub2"
                    );
                    test!(
                        "subsub1/file.txt",
                        "/test/sub1/subsub1/file.txt",
                        "/test/sub1/subsub1",
                        WorktreeScan
                    );
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/34027
        #[gpui::test]
        async fn issue_34027_non_worktree_local_file(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![
                    (
                        path!("/"),
                        json!({
                            "file.txt": "",
                        }),
                    ),
                    (
                        path!("/test"),
                        json!({
                            "file.txt": "",
                        }),
                    ),
                ],
                vec![path!("/test")],
                {
                    // Note: Opening a non-worktree file adds that file as a single file worktree.
                    test_local!("file.txt", "/file.txt", "/", BackgroundPathResolution);
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/34027
        #[gpui::test]
        async fn issue_34027_non_worktree_remote_file(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![
                    (
                        path!("/"),
                        json!({
                            "file.txt": "",
                        }),
                    ),
                    (
                        path!("/test"),
                        json!({
                            "file.txt": "",
                        }),
                    ),
                ],
                vec![path!("/test")],
                {
                    // Note: Opening a non-worktree file adds that file as a single file worktree.
                    test_remote!("file.txt", "/file.txt", "/", BackgroundPathResolution);
                    test_remote!("/test/file.txt", "/test/file.txt", "/");
                }
            )
        }

        // https://github.com/zed-industries/zed/issues/39159
        #[gpui::test]
        async fn issue_39159_remote_absolute_path_outside_worktree(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![
                    (
                        path!("/tmp"),
                        json!({
                            "a.txt": "",
                        }),
                    ),
                    (
                        path!("/code/project"),
                        json!({
                            "src": {
                                "lib.rs": "",
                            },
                        }),
                    ),
                ],
                vec![path!("/code/project")],
                {
                    test_remote!(
                        "/tmp/a.txt",
                        "/tmp/a.txt",
                        "/code/project",
                        BackgroundPathResolution
                    );
                }
            )
        }

        // See https://github.com/zed-industries/zed/issues/34027
        #[gpui::test]
        #[should_panic(expected = "Tooltip mismatch")]
        async fn issue_34027_gaps(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/project"),
                    json!({
                        "lib": {
                            "src": {
                                "main.rs": ""
                            },
                        },
                        "src": {
                            "main.rs": ""
                        },
                    }),
                )],
                vec![path!("/project")],
                {
                    test!("main.rs", "/project/src/main.rs", "/project");
                    test!("main.rs", "/project/lib/src/main.rs", "/project/lib");
                }
            )
        }

        // See https://github.com/zed-industries/zed/issues/34027
        #[gpui::test]
        #[should_panic(expected = "Tooltip mismatch")]
        async fn issue_34027_overlap(cx: &mut TestAppContext) {
            test_path_likes!(
                cx,
                vec![(
                    path!("/project"),
                    json!({
                        "lib": {
                            "src": {
                                "main.rs": ""
                            },
                        },
                        "src": {
                            "main.rs": ""
                        },
                    }),
                )],
                vec![path!("/project")],
                {
                    // Finds "/project/src/main.rs"
                    test!(
                        "src/main.rs",
                        "/project/lib/src/main.rs",
                        "/project/lib/src"
                    );
                }
            )
        }
    }

    // SOLO: of several files of a name, the one near the paths written around it.
    #[gpui::test]
    async fn test_scan_prefers_file_near_context_paths(cx: &mut TestAppContext) {
        let mut test_path_like = init_test(
            cx,
            vec![(
                path!("/godot"),
                json!({
                    "modules": {
                        "ai_generator": { "register_types.cpp": "" },
                        "dynamicbone": {
                            "register_types.cpp": "",
                            "runtime": { "dynamic_bone_collider_3d.h": "" },
                        },
                    },
                }),
            )],
            vec![path!("/godot")],
        )
        .await;
        let mut opened = Vec::new();
        for context_paths in [Vec::new(), vec!["dynamic_bone_collider_3d.h".to_string()]] {
            let (_, open_target) = test_path_like(
                HoveredWord {
                    word: "register_types.cpp:19".to_string(),
                    word_match: Range::new(Point::new(0, 0), Point::new(0, 0)),
                    id: 0,
                },
                PathLikeTarget {
                    maybe_path: "register_types.cpp:19".to_string(),
                    working_directory: Some(PathBuf::from(path!("/godot"))),
                    context_paths,
                    position_files: Vec::new(),
                },
                BackgroundPathChecks::LocalFileSystem,
            )
            .await;
            let open_target = open_target.expect("register_types.cpp should open");
            assert_eq!(open_target.found_by(), OpenTargetFoundBy::WorktreeScan);
            assert_eq!(open_target.path().row, Some(19));
            opened.push(open_target.path().path.clone());
        }
        assert_eq!(
            opened,
            [
                PathBuf::from(path!("/godot/modules/ai_generator/register_types.cpp")),
                PathBuf::from(path!("/godot/modules/dynamicbone/register_types.cpp")),
            ]
        );
    }

    /// SOLO: a file of `lines` lines.
    fn lines(lines: usize) -> String {
        "x\n".repeat(lines)
    }

    #[test]
    fn test_has_line() {
        let has = |text: &str, row| has_line(text.as_bytes(), row).unwrap();
        assert!(has("a\nb\nc\n", 3));
        assert!(!has("a\nb\nc\n", 4));
        assert!(has("a\nb\nc", 3));
        assert!(!has("a\nb\nc", 4));
        assert!(!has("", 2));
        assert!(has(&lines(5399), 3788));
        assert!(!has(&lines(2945), 3788));
    }

    // SOLO (G26): a bare position goes to the first file named before it that
    // exists and has the line, and the tooltip names the file a click opens.
    #[gpui::test]
    async fn test_bare_position_in_file_with_line(cx: &mut TestAppContext) {
        let mut test_path_like = init_test(
            cx,
            vec![(
                path!("/godot"),
                json!({
                    "core": { "object": { "class_db.cpp": lines(700) } },
                    "main": { "main.cpp": lines(5399) },
                    "modules": { "gdscript": { "gdscript.cpp": lines(2945) } },
                }),
            )],
            vec![path!("/godot")],
        )
        .await;
        let cases = [
            // Case one: gdscript.cpp has no line 3788.
            (
                "gdscript.cpp:3788",
                vec!["main.cpp:3788"],
                path!("/godot/main/main.cpp"),
                3788,
            ),
            // Case two: there's no file `classes.getptr`.
            (
                "classes.getptr:588",
                vec!["class_db.cpp:588"],
                path!("/godot/core/object/class_db.cpp"),
                588,
            ),
            (
                "classes.getptr:634",
                vec!["class_db.cpp:634"],
                path!("/godot/core/object/class_db.cpp"),
                634,
            ),
            // The nearest wins when it has the line, though others do too.
            (
                "class_db.cpp:600",
                vec!["main.cpp:600", "gdscript.cpp:600"],
                path!("/godot/core/object/class_db.cpp"),
                600,
            ),
            // None has the line: the nearest.
            (
                "class_db.cpp:6000",
                vec!["gdscript.cpp:6000", "main.cpp:6000"],
                path!("/godot/core/object/class_db.cpp"),
                6000,
            ),
        ];
        for (maybe_path, position_files, expected_path, expected_row) in cases {
            for background_path_checks in [
                BackgroundPathChecks::LocalFileSystem,
                BackgroundPathChecks::ProjectPathResolution,
            ] {
                let (hover_target, open_target) = test_path_like(
                    HoveredWord {
                        word: maybe_path.to_string(),
                        word_match: Range::new(Point::new(0, 0), Point::new(0, 0)),
                        id: 0,
                    },
                    PathLikeTarget {
                        maybe_path: maybe_path.to_string(),
                        working_directory: Some(PathBuf::from(path!("/godot"))),
                        context_paths: Vec::new(),
                        position_files: position_files.iter().map(|s| s.to_string()).collect(),
                    },
                    background_path_checks,
                )
                .await;
                let open_target = open_target.expect("a target should open");
                assert_eq!(
                    open_target.path().path,
                    Path::new(expected_path),
                    "{maybe_path}"
                );
                assert_eq!(open_target.path().row, Some(expected_row), "{maybe_path}");
                assert_eq!(
                    hover_target.expect("a tooltip should show").tooltip,
                    format!("{expected_path}:{expected_row}"),
                    "{maybe_path}"
                );
            }
        }
    }

    // SOLO (G10): a terminal in the solo tab whose claude works in
    // godot_engine, a folder no worktree holds, prints `object.h:163-164`.
    #[gpui::test]
    async fn test_bare_file_name_outside_the_worktrees(cx: &mut TestAppContext) {
        let mut test_path_like = init_test(
            cx,
            vec![
                (path!("/solo"), json!({ "README.md": "" })),
                (
                    path!("/godot"),
                    json!({
                        ".git": { "object.h": "" },
                        "core": { "object": { "object.h": lines(200) } },
                    }),
                ),
            ],
            vec![path!("/solo")],
        )
        .await;
        // Inside a worktree, only the worktrees are searched, as before. (First:
        // opening the file below adds it to the workspace.)
        for (working_directory, expected) in [
            (path!("/solo"), None),
            (path!("/godot"), Some(path!("/godot/core/object/object.h"))),
        ] {
            let (_, open_target) = test_path_like(
                HoveredWord {
                    word: "object.h:163".to_string(),
                    word_match: Range::new(Point::new(0, 0), Point::new(0, 0)),
                    id: 0,
                },
                PathLikeTarget {
                    maybe_path: "object.h:163".to_string(),
                    working_directory: Some(PathBuf::from(working_directory)),
                    context_paths: Vec::new(),
                    position_files: Vec::new(),
                },
                BackgroundPathChecks::LocalFileSystem,
            )
            .await;
            assert_eq!(
                open_target.as_ref().map(|target| target.path().path.as_path()),
                expected.map(Path::new),
                "{working_directory}"
            );
            if expected.is_some() {
                assert_eq!(open_target.unwrap().path().row, Some(163));
            }
        }
    }

    // SOLO (G26): an open buffer's lines count, not the file's on disk.
    #[gpui::test]
    async fn test_has_row_counts_open_buffer(cx: &mut TestAppContext) {
        let fs = cx.update(AppState::test).fs.as_fake().clone();
        fs.insert_tree(path!("/godot"), json!({ "a.cpp": lines(10) }))
            .await;
        let project = Project::test(fs, [Path::new(path!("/godot"))], cx).await;
        let target = OpenTarget::Path(
            util::paths::PathWithPosition {
                path: PathBuf::from(path!("/godot/a.cpp")),
                row: Some(15),
                column: None,
            },
            false,
            OpenTargetFoundBy::BackgroundPathResolution,
        );
        let mut async_cx = cx.to_async();
        assert!(!has_row(&target, &project, &mut async_cx).await);

        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/godot/a.cpp"), cx)
            })
            .await
            .unwrap();
        buffer.update(cx, |buffer, cx| {
            let end = buffer.len();
            buffer.edit([(end..end, lines(10))], None, cx);
        });
        assert!(has_row(&target, &project, &mut async_cx).await);
    }

    // SOLO (G26): how long resolving a bare position for its tooltip takes
    // (a click resolves it the same way, then opens the file) in a project
    // of Godot's size, with and without the files named further up. Run
    // with `--ignored --nocapture`.
    #[gpui::test]
    #[ignore]
    async fn bench_bare_position_resolution(cx: &mut TestAppContext) {
        let fs = cx.update(AppState::test).fs.as_fake().clone();
        cx.update(|cx| {
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let mut tree = serde_json::Map::new();
        for dir in 0..200 {
            let files = (0..100)
                .map(|file| (format!("file_{file}.cpp"), json!("")))
                .collect::<serde_json::Map<_, _>>();
            tree.insert(format!("dir_{dir}"), serde_json::Value::Object(files));
        }
        tree.insert(
            "core".to_string(),
            json!({ "object": { "class_db.cpp": lines(700) } }),
        );
        fs.insert_tree(path!("/godot"), serde_json::Value::Object(tree))
            .await;
        let project = Project::test(fs, [Path::new(path!("/godot"))], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().downgrade());
        cx.run_until_parked();

        let cases = [
            (
                "before G26: classes.getptr only",
                "classes.getptr:588",
                vec![],
            ),
            (
                "G26: classes.getptr, then class_db.cpp",
                "classes.getptr:588",
                vec!["class_db.cpp:588"],
            ),
            ("class_db.cpp only", "class_db.cpp:588", vec![]),
        ];
        for (label, maybe_path, position_files) in cases {
            let target = PathLikeTarget {
                maybe_path: maybe_path.to_string(),
                working_directory: Some(PathBuf::from(path!("/godot"))),
                context_paths: Vec::new(),
                position_files: position_files.iter().map(|s| s.to_string()).collect(),
            };
            let runs = 20;
            let start = std::time::Instant::now();
            for _ in 0..runs {
                cx.update(|_, cx| {
                    resolve_path_like_target(
                        &workspace,
                        &target,
                        cx,
                        BackgroundPathChecks::LocalFileSystem,
                    )
                })
                .await;
            }
            eprintln!("{label}: {:?} per resolution", start.elapsed() / runs);
        }
    }
}
