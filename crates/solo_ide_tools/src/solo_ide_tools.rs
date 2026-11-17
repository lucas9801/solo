//! SOLO: Solo's IDE tools for agents running in its terminals: a read-only
//! MCP server (streamable HTTP, on the loopback interface) with
//! `workspace_symbols`, `go_to_definition`, `find_references` and
//! `diagnostics`, answered by the language servers Solo already runs, so an
//! agent can ask them instead of grepping, without starting its own.
//!
//! Each workspace (project) gets its own random token the first time one of
//! its terminals starts, handed only to its terminals (see
//! `terminal::ide_tools`, which also passes the server to Claude Code and
//! Codex): a request answers only about the workspace whose token it
//! carries, and one without a valid token gets nothing. The server starts
//! with the first such terminal and runs until Solo quits. The
//! `terminal.ide_tools` setting turns it off: new terminals don't get it,
//! and requests from older ones are refused.

mod http;
mod mcp;
mod tools;

use std::{cell::RefCell, rc::Rc};

use collections::HashMap;
use futures::{StreamExt as _, channel::mpsc};
use gpui::{AnyWeakEntity, App, AsyncApp, EntityId, WeakEntity};
use project::Project;
use settings::Settings as _;
use terminal::{ide_tools::Endpoint, terminal_settings::TerminalSettings};

use crate::http::{Call, Reply};
use crate::tools::{IdeTools, OpenBuffers};

/// One workspace's tools.
struct Workspace {
    project: WeakEntity<Project>,
    open: OpenBuffers,
}

#[derive(Default)]
struct State {
    /// The server's URL, once it's started.
    url: Option<String>,
    tokens: HashMap<EntityId, String>,
    workspaces: HashMap<String, Workspace>,
}

pub fn init(cx: &mut App) {
    let state = Rc::new(RefCell::new(State::default()));
    cx.set_global(terminal::ide_tools::Provider(Rc::new(
        move |project: AnyWeakEntity, cx: &mut App| endpoint(&state, project, cx),
    )));
}

/// The endpoint of `project`'s workspace, starting the server and making
/// the workspace's token if need be.
fn endpoint(state: &Rc<RefCell<State>>, project: AnyWeakEntity, cx: &mut App) -> Option<Endpoint> {
    let project = project.upgrade()?.downcast::<Project>().ok()?;
    if state.borrow().url.is_none() {
        let (sender, receiver) = mpsc::unbounded();
        let url = http::start(sender)
            .map_err(|error| log::warn!("failed to start Solo's IDE tools server: {error}"))
            .ok()?;
        state.borrow_mut().url = Some(url);
        let state = state.clone();
        cx.spawn(async move |cx| serve(state, receiver, cx).await)
            .detach();
    }
    let mut state_ref = state.borrow_mut();
    let state_ref = &mut *state_ref;
    let id = project.entity_id();
    let token = match state_ref.tokens.get(&id) {
        Some(token) => token.clone(),
        None => {
            let token = random_token();
            state_ref.tokens.insert(id, token.clone());
            state_ref.workspaces.insert(
                token.clone(),
                Workspace {
                    project: project.downgrade(),
                    open: OpenBuffers::default(),
                },
            );
            let state = state.clone();
            cx.observe_release(&project, move |_, _| {
                let mut state = state.borrow_mut();
                if let Some(token) = state.tokens.remove(&id) {
                    state.workspaces.remove(&token);
                }
            })
            .detach();
            token
        }
    };
    Some(Endpoint {
        url: state_ref.url.clone()?,
        token,
    })
}

/// Answers the server's calls, each in its own task.
async fn serve(
    state: Rc<RefCell<State>>,
    mut calls: mpsc::UnboundedReceiver<Call>,
    cx: &mut AsyncApp,
) {
    while let Some(call) = calls.next().await {
        let state = state.clone();
        cx.spawn(async move |cx| {
            let reply = answer(&state, call.token, call.messages, cx).await;
            call.reply.send(reply).ok();
        })
        .detach();
    }
}

async fn answer(
    state: &Rc<RefCell<State>>,
    token: Option<String>,
    messages: serde_json::Value,
    cx: &mut AsyncApp,
) -> Reply {
    let workspace = token.and_then(|token| {
        let state = state.borrow();
        let workspace = state.workspaces.get(&token)?;
        Some((workspace.project.upgrade()?, workspace.open.clone()))
    });
    let Some((project, open)) = workspace else {
        return Reply::Unauthorized;
    };
    if !cx.update(|cx| TerminalSettings::get_global(cx).ide_tools) {
        return Reply::Forbidden(
            "Solo's IDE tools are turned off (the terminal.ide_tools setting).",
        );
    }
    let instructions = cx.update(|cx| instructions(&project, cx));
    let tools = IdeTools::new(project, open);
    let (batch, messages) = match messages {
        serde_json::Value::Array(messages) => (true, messages),
        message => (false, vec![message]),
    };
    let mut answers = Vec::new();
    for message in &messages {
        match mcp::dispatch(message, &instructions) {
            mcp::Incoming::Respond(answer) => answers.push(answer),
            mcp::Incoming::Nothing => {}
            mcp::Incoming::CallTool {
                id,
                name,
                arguments,
            } => {
                let output = tools.call(&name, arguments, cx).await;
                answers.push(mcp::tool_result(id, output));
            }
        }
    }
    match (batch, answers.len()) {
        (_, 0) => Reply::Accepted,
        (false, _) => Reply::Json(answers.swap_remove(0)),
        (true, _) => Reply::Json(serde_json::Value::Array(answers)),
    }
}

/// What the server tells agents when they connect: what the tools are for,
/// and where the workspace is.
fn instructions(project: &gpui::Entity<Project>, cx: &App) -> String {
    let roots: Vec<String> = project
        .read(cx)
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_string_lossy().into_owned())
        .collect();
    format!(
        "Solo's IDE tools: the language servers of the Solo workspace this terminal belongs to ({}). \
         To find where something is defined or used, prefer workspace_symbols, go_to_definition and \
         find_references to grep: they follow the code, not the text. Paths are relative to the \
         workspace root. Read-only.",
        roots.join(", ")
    )
}

/// 128 random bits, in hex.
fn random_token() -> String {
    use rand::{Rng as _, SeedableRng as _};
    let bytes: [u8; 16] = rand::rngs::StdRng::from_os_rng().random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
