//! SOLO: showing Mermaid diagrams printed in the terminal rendered, in the
//! Mermaid viewer. Clicking a diagram's `mermaid` line opens it; so does
//! the "Preview Mermaid Diagram" action, for the last one.

use gpui::{Context, Entity, Window, actions};
use terminal::{HoveredWord, Terminal};

use crate::{TerminalView, mermaid_viewer::MermaidViewer};

actions!(
    terminal,
    [
        /// Opens the last Mermaid diagram printed in the terminal, rendered.
        PreviewMermaidDiagram,
    ]
);

pub(crate) fn mermaid_tooltip() -> &'static str {
    util::i18n::tr("Open Mermaid diagram", "打开 Mermaid 图")
}

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

/// Assistant messages searched for a diagram's source, latest first.
const ANSWERS_SEARCHED: usize = 20;
/// Lines of the diagram on screen compared with each source.
const LINES_COMPARED: usize = 8;

/// The ```` ```mermaid ```` code blocks in an agent's answer, in order.
pub(crate) fn mermaid_blocks(answer: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = answer.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let fence_len = trimmed
            .chars()
            .take_while(|char| *char == '`' || *char == '~')
            .count();
        if fence_len < 3 || !trimmed[fence_len..].trim().eq_ignore_ascii_case("mermaid") {
            continue;
        }
        let fence_char = trimmed.chars().next().unwrap_or('`');
        let indent = line.len() - trimmed.len();
        let mut block = Vec::new();
        for line in lines.by_ref() {
            let closing = line.trim();
            if closing.len() >= fence_len && closing.chars().all(|char| char == fence_char) {
                break;
            }
            // Within a list item, the block is indented as its fence is.
            let strip = line
                .char_indices()
                .take_while(|(ix, char)| *ix < indent && char.is_whitespace())
                .count();
            block.push(&line[strip..]);
        }
        if block.iter().any(|line| !line.trim().is_empty()) {
            blocks.push(block.join(
                "
",
            ));
        }
    }
    blocks
}

/// A line as compared between the screen and the source: the screen shows it
/// indented differently, and may wrap it or cut it off.
fn comparable(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The source of a diagram in an agent's answers (latest first): the one
/// whose first lines are those of the part on screen, or without one, the
/// latest. Ties go to the latest.
pub(crate) fn diagram_in_answers<'a>(
    answers: impl IntoIterator<Item = &'a str>,
    on_screen: Option<&str>,
) -> Option<String> {
    let screen_lines: Vec<String> = on_screen
        .into_iter()
        .flat_map(str::lines)
        .map(comparable)
        .filter(|line| !line.is_empty())
        .take(LINES_COMPARED)
        .collect();
    let mut best: Option<(usize, String)> = None;
    for answer in answers {
        // The last diagram of an answer is the likeliest.
        for block in mermaid_blocks(answer).into_iter().rev() {
            if screen_lines.is_empty() {
                return Some(block);
            }
            let source_lines = block
                .lines()
                .map(comparable)
                .filter(|line| !line.is_empty());
            let matching = screen_lines
                .iter()
                .zip(source_lines)
                .take_while(|(screen, source)| {
                    source == *screen || (screen.len() > 3 && source.starts_with(screen.as_str()))
                })
                .count();
            if matching > 0 && best.as_ref().is_none_or(|(most, _)| matching > *most) {
                best = Some((matching, block));
            }
        }
    }
    best.map(|(_, block)| block)
}

impl TerminalView {
    pub(crate) fn preview_mermaid_diagram(
        &mut self,
        _: &PreviewMermaidDiagram,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_screen = self.terminal.read(cx).mermaid_diagram(None);
        self.open_mermaid_diagram_from_session(on_screen, window, cx);
    }

    fn show_mermaid_toast(&self, message: &'static str, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_toast(
                    workspace::Toast::new(
                        workspace::notifications::NotificationId::unique::<PreviewMermaidDiagram>(),
                        message,
                    )
                    .autohide(),
                    cx,
                )
            });
        }
    }

    /// Opens the diagram whose `mermaid` line was clicked, taken from the
    /// screen.
    pub(crate) fn open_mermaid_diagram(
        &mut self,
        diagram: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_mermaid_diagram_from_session(Some(diagram), window, cx);
    }

    /// Opens a diagram from the session record of the agent in the terminal,
    /// which has all of it where the screen may show only part (a full-screen
    /// agent shows what fits), or else from the screen.
    fn open_mermaid_diagram_from_session(
        &mut self,
        on_screen: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let find = self.find_agent_session(cx);
        cx.spawn_in(window, async move |this, cx| {
            let answers = match find.await {
                Some(session) => {
                    crate::agent_session::agent_answers(&session, ANSWERS_SEARCHED, cx).await
                }
                None => None,
            };
            let from_session = answers.and_then(|answers| {
                diagram_in_answers(answers.iter().map(String::as_str), on_screen.as_deref())
            });
            this.update_in(cx, |this, window, cx| match (from_session, on_screen) {
                (Some(diagram), _) => this.show_mermaid_diagram(diagram, window, cx),
                // Part of a diagram taken from the screen may have been
                // scrolled off it; previewing what's left would only show it
                // failing to render.
                (None, Some(diagram)) if !terminal::mermaid_diagram_is_complete(&diagram) => this
                    .show_mermaid_toast(
                        util::i18n::tr(
                            "The Mermaid diagram is incomplete (part of it may be off screen)",
                            "Mermaid 图不完整（部分内容可能不在屏幕上）",
                        ),
                        cx,
                    ),
                (None, Some(diagram)) => this.show_mermaid_diagram(diagram, window, cx),
                (None, None) => this.show_mermaid_toast(
                    util::i18n::tr(
                        "No Mermaid diagram in the terminal output",
                        "终端输出里没有 Mermaid 图",
                    ),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    fn show_mermaid_diagram(
        &mut self,
        diagram: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        // Opening the viewer deactivates this view, which can't be updated
        // while it's being updated here.
        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                MermaidViewer::open(workspace, diagram, window, cx);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long diagram, as an agent writes it in its answer.
    fn long_diagram() -> String {
        let mut lines = vec!["graph TB".to_string()];
        lines.push("    subgraph Scene[\"Scene layer\"]".to_string());
        for ix in 0..80 {
            lines.push(format!(
                "        Node{ix}[\"Node {ix}\"] --> Node{}",
                ix + 1
            ));
        }
        lines.push("    end".to_string());
        lines.join(
            "
",
        )
    }

    #[test]
    fn test_mermaid_blocks() {
        let answer = "Intro

```mermaid
graph TD
  A --> B
```

```rust
fn main() {}
```

1. Item
   ````Mermaid
   sequenceDiagram
     A->>B: hi
   ````

```mermaid
```
~~~mermaid
pie
  \"a\": 1";
        assert_eq!(
            mermaid_blocks(answer),
            [
                "graph TD
  A --> B",
                "sequenceDiagram
  A->>B: hi",
                "pie
  \"a\": 1",
            ]
        );
    }

    #[test]
    fn test_diagram_in_answers() {
        let long = long_diagram();
        assert_eq!(long.lines().count(), 83);
        let older = format!(
            "Before:

```mermaid
{long}
```"
        );
        let latest = "Now:

```mermaid
graph TB
    A --> B
```

and

```mermaid
sequenceDiagram
    A->>B: hi
```";
        let answers = [latest, older.as_str()];

        // What a full-screen agent shows of it: the first lines, indented and
        // with a line cut off at the edge.
        let on_screen: Vec<String> = long
            .lines()
            .take(12)
            .enumerate()
            .map(|(ix, line)| {
                let line = if ix == 11 { &line[..20] } else { line };
                format!("  {}", line.trim_start())
            })
            .collect();
        let on_screen = on_screen.join(
            "
",
        );
        assert_eq!(
            diagram_in_answers(answers, Some(&on_screen)).as_deref(),
            Some(long.as_str())
        );

        // Without a diagram on screen, the latest answer's last one.
        assert_eq!(
            diagram_in_answers(answers, None).as_deref(),
            Some(
                "sequenceDiagram
    A->>B: hi"
            )
        );
        // Only the first line in common: the latest such.
        assert_eq!(
            diagram_in_answers(
                answers,
                Some(
                    "graph TB
  X --> Y"
                )
            )
            .as_deref(),
            Some(
                "graph TB
    A --> B"
            )
        );
        // Not in the answers: taken from the screen instead.
        assert_eq!(
            diagram_in_answers(
                answers,
                Some(
                    "classDiagram
  A <|-- B"
                )
            ),
            None
        );
        assert_eq!(diagram_in_answers(["no diagrams"], None), None);
    }
}
