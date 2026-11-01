//! SOLO: showing Mermaid diagrams printed in the terminal rendered, in the
//! Markdown preview. Clicking a diagram's `mermaid` line opens it; so does
//! the "Preview Mermaid Diagram" action, for the last one.

use std::{
    hash::{DefaultHasher, Hash as _, Hasher as _},
    path::PathBuf,
};

use gpui::{AppContext as _, Context, Entity, Window, actions};
use terminal::{HoveredWord, Terminal};
use util::ResultExt as _;
use workspace::{OpenOptions, OpenVisible};

use crate::TerminalView;

actions!(
    terminal,
    [
        /// Opens the last Mermaid diagram printed in the terminal, rendered.
        PreviewMermaidDiagram,
    ]
);

pub(crate) const MERMAID_TOOLTIP: &str = "Open Mermaid diagram";

/// The diagram below `word`, if that's a diagram's `mermaid` line.
pub(crate) fn diagram_at_word(
    terminal: &Entity<Terminal>,
    word: &HoveredWord,
    cx: &gpui::App,
) -> Option<String> {
    if !word.word.eq_ignore_ascii_case("mermaid") {
        return None;
    }
    terminal
        .read(cx)
        .mermaid_diagram(Some(word.word_match.start().line))
}

/// Where a diagram is written to be previewed; the same diagram reuses its file.
fn diagram_path(diagram: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    diagram.hash(&mut hasher);
    paths::temp_dir()
        .join("diagrams")
        .join(format!("mermaid-{:08x}.md", hasher.finish() as u32))
}

impl TerminalView {
    pub(crate) fn preview_mermaid_diagram(
        &mut self,
        _: &PreviewMermaidDiagram,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.terminal.read(cx).mermaid_diagram(None) {
            Some(diagram) => self.open_mermaid_diagram(diagram, window, cx),
            None => {
                if let Some(workspace) = self.workspace.upgrade() {
                    workspace.update(cx, |workspace, cx| {
                        workspace.show_toast(
                            workspace::Toast::new(
                                workspace::notifications::NotificationId::unique::<
                                    PreviewMermaidDiagram,
                                >(),
                                "No Mermaid diagram in the terminal output",
                            )
                            .autohide(),
                            cx,
                        )
                    });
                }
            }
        }
    }

    pub(crate) fn open_mermaid_diagram(
        &mut self,
        diagram: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = diagram_path(&diagram);
        let workspace = self.workspace.clone();
        let write = cx.background_spawn({
            let path = path.clone();
            async move {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, format!("```mermaid\n{diagram}\n```\n"))
            }
        });
        cx.spawn_in(window, async move |_, cx| {
            write.await.log_err()?;
            let open = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_paths(
                        vec![path],
                        OpenOptions {
                            visible: Some(OpenVisible::None),
                            ..Default::default()
                        },
                        None,
                        window,
                        cx,
                    )
                })
                .ok()?;
            for item in open.await.into_iter().flatten() {
                item.log_err();
            }
            Some(())
        })
        .detach();
    }
}
