//! SOLO: a smoke test that dispatches every action Solo adds, the way a user
//! reaches them, and only checks that nothing panics or hangs. Feature tests
//! that only call pure functions missed crashes such as the Agent Timeline
//! one (G1), which panicked as soon as the tab menu entry was chosen.
//!
//! Solo's actions are the registered ones Zed didn't have: the list of Zed's
//! comes from `script/solo-feature-audit/upstream-actions.txt`, so a new
//! action is dispatched here without changing this test.

use std::{collections::BTreeSet, io::Write as _, path::PathBuf};

use agent_sessions::{AgentKind, MatchedBy, SessionLocation};
use editor::Editor;
use gpui::{
    Action, App, AppContext as _, Entity, FocusHandle, Focusable as _, TestAppContext,
    VisualTestContext,
};
use project::{FakeFs, Project};
use settings::SettingsStore;
use terminal::Terminal;
use util::{path, paths::PathStyle};
use workspace::{
    MultiWorkspace, OpenOptions, Workspace,
    item::{Item as _, ItemHandle},
};

use crate::TerminalView;

const UPSTREAM_ACTIONS: &str =
    include_str!("../../../script/solo-feature-audit/upstream-actions.txt");

/// Actions not dispatched, with why. They're still built.
const NOT_DISPATCHED: &[(&str, &str)] = &[
    (
        "terminal::OpenWorkflowsFile",
        "creates workflows.json in the real config directory",
    ),
    (
        "terminal::SaveCommandBlockAsWorkflow",
        "writes workflows.json in the real config directory",
    ),
];

/// The namespaces of the actions this crate adds: each of its Solo actions
/// must be dispatched (or listed in NOT_DISPATCHED).
const OWN_NAMESPACES: &[&str] = &["terminal::", "agent_task::", "agent_board::"];

const NONCE: &str = "smoke-nonce";

/// Claude Code started at a prompt and reported a turn through its hooks.
const AGENT_OUTPUT: &[u8] = b"\x1b]633;P;Shell=solo-test-shell;smoke-nonce\x07\
\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]633;E;claude;smoke-nonce\x07\x1b]133;C\x07\
\x1b[?2004h\x1b]6973;agent;working;1;;Claude Code\x07Working\r\n\
\x1b]6973;agent;stop;2;;Claude Code\x07Done\r\n";

/// A failed build, a command that printed a Mermaid diagram, and the shell
/// waiting at its prompt (so the input editor shows).
const PLAIN_OUTPUT: &[u8] = b"\x1b]633;P;Shell=solo-test-shell;smoke-nonce\x07\
\x1b]133;A\x07$ \x1b]133;B\x07cargo build\r\n\x1b]633;E;cargo build;smoke-nonce\x07\x1b]133;C\x07\
error[E0308]: mismatched types\r\n --> src/main.rs:2:5\r\n\x1b]133;D;101\x07\
\x1b]133;A\x07$ \x1b]133;B\x07cat graph.md\r\n\x1b]633;E;cat graph.md;smoke-nonce\x07\x1b]133;C\x07\
```mermaid\r\ngraph TD\r\n  A --> B\r\n```\r\n\x1b]133;D;0\x07\
\x1b]133;A\x07$ \x1b]133;B\x07";

/// A made-up Claude Code transcript: a prompt, a file read and edited, and
/// an answer.
const TRANSCRIPT: &str = r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"Fix the build"},"uuid":"u1","timestamp":"2026-11-01T13:44:31.800Z","sessionId":"s1"}
{"parentUuid":"u1","isSidechain":false,"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_1","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"src/main.rs"}},{"type":"tool_use","id":"toolu_2","name":"Edit","input":{"file_path":"src/main.rs","old_string":"a","new_string":"b"}}],"usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":32912,"output_tokens":305}},"uuid":"a1","timestamp":"2026-11-01T13:44:36.229Z"}
{"parentUuid":"a1","isSidechain":false,"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"fn main() {}"},{"tool_use_id":"toolu_2","type":"tool_result","content":"ok"}]},"uuid":"u2","timestamp":"2026-11-01T13:44:40.000Z"}
{"parentUuid":"u2","isSidechain":false,"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_2","type":"message","role":"assistant","content":[{"type":"text","text":"Fixed:\n\n```mermaid\ngraph TD\n  A --> B\n```"}],"stop_reason":"end_turn","usage":{"input_tokens":2,"cache_creation_input_tokens":10,"cache_read_input_tokens":46685,"output_tokens":365}},"uuid":"a2","timestamp":"2026-11-01T13:45:10.457Z"}
"#;

/// Solo's actions registered in this test binary, by name.
fn solo_action_names(cx: &TestAppContext) -> Vec<&'static str> {
    let upstream: BTreeSet<String> = UPSTREAM_ACTIONS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        // Solo renamed Zed's `zed` namespace to `solo`.
        .flat_map(|line| {
            let renamed = line
                .strip_prefix("zed::")
                .map(|name| format!("solo::{name}"));
            std::iter::once(line.to_string()).chain(renamed)
        })
        .collect();
    let mut names: Vec<&'static str> = cx.update(|cx| {
        cx.all_action_names()
            .iter()
            .copied()
            .filter(|name| !upstream.contains(*name))
            .collect()
    });
    names.sort_unstable();
    names
}

/// The variants of an action worth dispatching: with its default data, and
/// for those that take options, with each one on.
fn action_variants(name: &str, cx: &mut VisualTestContext) -> Vec<Box<dyn Action>> {
    let options: &[serde_json::Value] = match name {
        "terminal::SendSelectionToAgent"
        | "terminal::SendPositionToAgent"
        | "terminal::SendDiagnosticsToAgent" => &[serde_json::json!({ "choose_terminal": true })],
        _ => &[],
    };
    cx.update(|_, cx| {
        std::iter::once(None)
            .chain(options.iter().cloned().map(Some))
            .map(|data| {
                cx.build_action(name, data)
                    .unwrap_or_else(|error| panic!("can't build {name}: {error}"))
            })
            .collect()
    })
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

/// Where an action is dispatched from: an item, shown in its pane, and the
/// focus in it.
struct Target {
    name: &'static str,
    item: Box<dyn ItemHandle>,
    focus: FocusHandle,
    /// Where the focus may also go when `focus` gets it, such as the
    /// terminal's input editor while the shell waits at its prompt.
    focus_moves_to: Option<FocusHandle>,
}

fn dispatch_from(
    target: &Target,
    action: &dyn Action,
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) {
    // As a menu does: show the item (a focus handle that isn't drawn gets
    // no actions), then focus it.
    let activated = workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(target.item.as_ref(), true, true, window, cx)
    });
    draw(cx);
    cx.update(|window, cx| window.focus(&target.focus, cx));
    draw(cx);
    let (focused, debug) = cx.update(|window, cx| {
        (
            target
                .item
                .item_focus_handle(cx)
                .contains_focused(window, cx)
                || target
                    .focus_moves_to
                    .as_ref()
                    .is_some_and(|handle| handle.contains_focused(window, cx)),
            format!(
                "activated {activated}, focused {:?}, target {:?}, item {:?}",
                window.focused(cx),
                target.focus,
                target.item.item_focus_handle(cx)
            ),
        )
    });
    assert!(
        focused,
        "{} has the focus for {}: {debug}",
        target.name,
        action.name()
    );
    cx.update(|window, cx| window.dispatch_action(action.boxed_clone(), cx));
    draw(cx);
    // Close what the action opened, so the next one starts from the same
    // place; a modal would otherwise take the focus.
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.hide_modal(window, cx);
    });
    draw(cx);
}

fn add_terminal(
    workspace: &Entity<Workspace>,
    project: &Entity<Project>,
    output: &[u8],
    cx: &mut VisualTestContext,
) -> (Entity<Terminal>, Entity<TerminalView>) {
    let (terminal, view) = workspace.update_in(cx, |workspace, window, cx| {
        let terminal = cx.new(|cx| {
            let mut terminal = terminal::TerminalBuilder::new_display_only(
                Default::default(),
                terminal::terminal_settings::AlternateScroll::On,
                None,
                0,
                cx.background_executor(),
                PathStyle::local(),
            )
            .subscribe(cx);
            terminal.set_shell_nonce(NONCE);
            terminal
        });
        let workspace_handle = cx.weak_entity();
        let view = cx.new(|cx| {
            TerminalView::new(
                terminal.clone(),
                workspace_handle,
                None,
                project.downgrade(),
                window,
                cx,
            )
        });
        workspace.add_item_to_active_pane(Box::new(view.clone()), None, true, window, cx);
        (terminal, view)
    });
    terminal.update(cx, |terminal, cx| terminal.write_raw_output(output, cx));
    draw(cx);
    (terminal, view)
}

#[gpui::test]
async fn test_dispatch_every_solo_action(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let mut store = SettingsStore::test(cx);
        store.update_user_settings(cx, |settings| {
            let terminal = settings.terminal.get_or_insert_default();
            terminal.starts_open = Some(false);
            // Terminals an action opens mustn't start a real shell.
            terminal.project.shell =
                Some(settings::Shell::Program("__nonexistent_shell__".to_owned()));
        });
        cx.set_global(store);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);
        crate::init(cx);
    });
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/project"),
        serde_json::json!({
            "src": { "main.rs": "fn main() {\n    let x: u32 = \"a\";\n}\n" },
            "README.md": "# Project\n\nSome text.\n",
        }),
    )
    .await;
    let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
    let window_handle =
        cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
    let workspace = window_handle
        .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone())
        .unwrap();
    let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

    let editor = workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_abs_path(
                PathBuf::from(path!("/project/src/main.rs")),
                OpenOptions::default(),
                window,
                cx,
            )
        })
        .await
        .unwrap()
        .downcast::<Editor>()
        .expect("an editor");
    editor.update_in(cx, |editor, window, cx| {
        editor.select_all(&editor::actions::SelectAll, window, cx)
    });

    let (_, agent_view) = add_terminal(&workspace, &project, AGENT_OUTPUT, cx);
    assert!(agent_view.read_with(cx, |view, cx| view.has_run_agent(cx)));
    // The session Claude Code recorded, as if chosen with Choose Agent
    // Session (the real one is looked up in ~/.claude).
    let transcript_dir = tempfile::tempdir().unwrap();
    let transcript = transcript_dir.path().join("s1.jsonl");
    std::fs::File::create(&transcript)
        .and_then(|mut file| file.write_all(TRANSCRIPT.as_bytes()))
        .unwrap();
    agent_view
        .update(cx, |view, cx| view.find_agent_session(cx))
        .await;
    agent_view.update(cx, |view, cx| {
        view.agent_session_chosen(
            SessionLocation {
                agent: AgentKind::ClaudeCode,
                session_id: "s1".into(),
                path: transcript,
                modified: None,
                matched_by: MatchedBy::Chosen,
            },
            cx,
        )
    });
    draw(cx);

    let (_, plain_view) = add_terminal(&workspace, &project, PLAIN_OUTPUT, cx);
    let input = plain_view
        .read_with(cx, |view, _| view.input.clone())
        .expect("the input editor shows at the prompt");

    let target = |name, view: &Entity<TerminalView>, focus, cx: &App| Target {
        name,
        item: Box::new(view.clone()),
        focus,
        focus_moves_to: view
            .read(cx)
            .input
            .as_ref()
            .map(|input| input.read(cx).focus_handle(cx)),
    };
    let targets: Vec<Target> = cx.update(|_, cx| {
        vec![
            target(
                "agent terminal",
                &agent_view,
                agent_view.read(cx).focus_handle.clone(),
                cx,
            ),
            target(
                "plain terminal",
                &plain_view,
                plain_view.read(cx).focus_handle.clone(),
                cx,
            ),
            target(
                "input editor",
                &plain_view,
                input.read(cx).focus_handle(cx),
                cx,
            ),
            Target {
                name: "editor",
                item: Box::new(editor.clone()),
                focus: editor.read(cx).focus_handle(cx),
                focus_moves_to: None,
            },
        ]
    });

    // The tab menus' own entries, from their terminals, as choosing them does.
    for (view, target) in [(&agent_view, &targets[0]), (&plain_view, &targets[1])] {
        let entries = view.update_in(cx, |view, window, cx| {
            view.tab_extra_context_menu_actions(window, cx)
        });
        for (_, action) in &entries {
            dispatch_from(target, action.as_ref(), &workspace, cx);
        }
        if view == &agent_view {
            assert!(
                entries
                    .iter()
                    .any(|(_, action)| action.name() == "terminal::ShowAgentTimeline"),
                "the agent terminal's tab menu has the Agent Timeline"
            );
        }
    }

    assert_eq!(
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .items_of_type::<crate::agent_timeline::AgentTimeline>(cx)
                .count()
        }),
        1,
        "the tab menu opened the agent's timeline"
    );

    let names = solo_action_names(cx);
    let mut dispatched = BTreeSet::new();
    for name in &names {
        let skipped = NOT_DISPATCHED.iter().any(|(skipped, _)| skipped == name);
        for action in action_variants(name, cx) {
            if skipped {
                continue;
            }
            for target in &targets {
                dispatch_from(target, action.as_ref(), &workspace, cx);
            }
        }
        if !skipped {
            dispatched.insert(*name);
        }
    }

    // Registered with `no_register`, so it can't be built by name: a
    // Markdown preview builds it from the comments on a document.
    let comments = zed_actions::agent_terminal::SendDocumentCommentsToAgent {
        path: PathBuf::from(path!("/project/README.md")),
        comments: vec![zed_actions::agent_terminal::DocumentComment {
            start_line: 3,
            end_line: 3,
            quote: "Some text.".into(),
            comment: "Say more".into(),
        }],
    };
    for target in &targets {
        dispatch_from(target, &comments, &workspace, cx);
    }

    // Everything this crate adds was reached, so the baseline file was read
    // and nothing fell out of the registry.
    let own: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| {
            OWN_NAMESPACES
                .iter()
                .any(|namespace| name.starts_with(namespace))
        })
        .collect();
    assert!(
        own.len() >= 30,
        "expected Solo's terminal actions, found {own:?}"
    );
    for name in own {
        assert!(
            dispatched.contains(name) || NOT_DISPATCHED.iter().any(|(skipped, _)| *skipped == name),
            "{name} wasn't dispatched"
        );
    }
}
