//! SOLO: Mermaid diagrams printed into the terminal, such as one an agent
//! drew in its answer, to be shown rendered.
//!
//! A diagram starts below a `mermaid` line: a code fence (```` ```mermaid ````)
//! or, as Claude Code draws code blocks, the bare language name.

use unicode_width::UnicodeWidthStr as _;

const DIAGRAM_TYPES: &[&str] = &[
    "graph",
    "flowchart",
    "sequenceDiagram",
    "classDiagram",
    "stateDiagram",
    "erDiagram",
    "journey",
    "gantt",
    "pie",
    "gitGraph",
    "mindmap",
    "timeline",
    "quadrantChart",
    "requirementDiagram",
    "C4Context",
    "C4Container",
    "C4Component",
    "C4Dynamic",
    "C4Deployment",
    "sankey",
    "xychart",
    "block",
    "packet",
    "architecture",
    "kanban",
    "radar",
    "---",
    "%%",
];

/// Whether `line` starts a diagram, and if so whether with a code fence.
fn header(line: &str) -> Option<bool> {
    // Agents put bullets like `●` before their output.
    let line = line
        .trim_start_matches(|c: char| c.is_whitespace() || !c.is_ascii() || c == '*' || c == '-')
        .trim_end();
    match line {
        "mermaid" => Some(false),
        "```mermaid" | "~~~mermaid" => Some(true),
        _ => None,
    }
}

pub(crate) fn is_header(line: &str) -> bool {
    header(line).is_some()
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Text that Claude Code draws over its output, as the pill offering to
/// scroll back down, and that is not part of what it printed.
const OVERLAYS: &[&str] = &["Jump to bottom", " new message"];

/// `line` without the text another program drew over it; `None` if that's
/// all there is on it.
fn without_overlay(line: &str) -> Option<String> {
    let Some(overlay_start) = OVERLAYS.iter().find_map(|overlay| line.find(overlay)) else {
        return Some(line.to_string());
    };
    // The pill reads `Jump to bottom (ctrl+End) ↓` or `3 new messages: fn+↓ to scroll`.
    let rest = &line[overlay_start..];
    let Some(overlay_len) = ["↓", "to scroll"]
        .iter()
        .filter_map(|end| rest.find(end).map(|ix| ix + end.len()))
        .max()
    else {
        return Some(line.to_string());
    };
    let before = line[..overlay_start].trim_end_matches(|c: char| c.is_ascii_digit());
    let after = &line[overlay_start + overlay_len..];
    let line = format!("{}{}", before.trim_end(), after.trim_end());
    (!line.trim().is_empty()).then_some(line)
}

/// How much narrower than the terminal a program may wrap its output.
const WRAP_MARGIN: usize = 4;

/// Whether `next` continues `previous`, a line that a program running full
/// screen, like Claude Code, wrapped itself, so the terminal didn't mark it
/// as wrapped: `previous` fills the row, so that the first word of `next`
/// wouldn't fit after it, and `next` isn't indented like a line of the diagram.
fn continues(previous: &str, next: &str, base: usize, columns: usize) -> bool {
    if columns == 0 || previous.trim().is_empty() || next.trim().is_empty() || indent(next) > base {
        return false;
    }
    let first_word = next.trim_start().split(' ').next().unwrap_or_default();
    last_row_width(previous, columns) + 1 + first_word.width() > columns.saturating_sub(WRAP_MARGIN)
}

/// The width of the last row `line` takes up in a terminal `columns` wide.
fn last_row_width(line: &str, columns: usize) -> usize {
    match line.width() {
        0 => 0,
        width => (width - 1) % columns.max(1) + 1,
    }
}

/// The diagram whose header is `lines[header_ix]`, without the indentation it
/// was printed with. `columns` is the terminal's width, to tell where a line
/// was wrapped.
pub(crate) fn diagram_after(lines: &[&str], header_ix: usize, columns: usize) -> Option<String> {
    let fenced = header(lines.get(header_ix)?)?;
    let body: Vec<String> = lines[header_ix + 1..]
        .iter()
        .filter_map(|line| without_overlay(line))
        .collect();
    let first = body.iter().position(|line| !line.trim().is_empty())?;
    let base = indent(&body[first]);
    let start = body[first].trim_start();
    if !DIAGRAM_TYPES.iter().any(|kind| start.starts_with(kind)) {
        return None;
    }
    let mut joined: Vec<String> = Vec::with_capacity(body.len() - first);
    let mut previous_row: Option<&str> = None;
    for line in &body[first..] {
        match (previous_row, joined.last_mut()) {
            (Some(previous), Some(last)) if !fenced && continues(previous, line, base, columns) => {
                // A word longer than the row is cut where the row ends; others
                // move to the next row, and the space before them is left out.
                if last_row_width(previous, columns) < columns {
                    last.push(' ');
                }
                last.push_str(line.trim_start());
            }
            _ => joined.push(line.clone()),
        }
        previous_row = Some(line);
    }
    let body = joined;
    let rest = 1;
    let end = if fenced {
        body[rest..]
            .iter()
            .position(|line| {
                let line = line.trim_start();
                line.starts_with("```") || line.starts_with("~~~")
            })
            .map_or(body.len(), |ix| rest + ix)
    } else {
        // Without a fence the diagram ends where the text after it starts:
        // at the first line indented no deeper than its first line, or, if
        // the diagram itself isn't indented, at the first blank line.
        let shallower = body[rest..]
            .iter()
            .position(|line| !line.trim().is_empty() && indent(line) <= base)
            .map_or(body.len(), |ix| rest + ix);
        if body[rest..shallower]
            .iter()
            .any(|line| !line.trim().is_empty())
        {
            shallower
        } else {
            body[rest..]
                .iter()
                .position(|line| line.trim().is_empty())
                .map_or(body.len(), |ix| rest + ix)
        }
    };
    let mut diagram: Vec<&str> = body[..end]
        .iter()
        .map(|line| &line[indent(line).min(base)..])
        .collect();
    while diagram.last().is_some_and(|line| line.trim().is_empty()) {
        diagram.pop();
    }
    Some(diagram.join("\n"))
}

/// Whether `diagram`, as taken from the terminal, looks whole, as part of it
/// may have been off screen. It isn't if a string or a flowchart node's
/// brackets are left open, a `subgraph` or a block of messages has no `end`,
/// or a flowchart's last line stops after an arrow.
pub fn is_complete(diagram: &str) -> bool {
    let lines: Vec<&str> = diagram
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("%%"))
        .collect();
    let Some(kind) = lines.first() else {
        return false;
    };
    let is_flowchart = kind.starts_with("graph") || kind.starts_with("flowchart");
    let blocks: &[&str] = if is_flowchart {
        &["subgraph"]
    } else if kind.starts_with("sequenceDiagram") {
        &[
            "loop", "alt", "opt", "par", "critical", "break", "rect", "box",
        ]
    } else {
        &[]
    };

    let mut in_string = false;
    let mut open_brackets = Vec::new();
    let mut open_blocks = 0usize;
    for line in &lines {
        let keyword = line
            .split(|c: char| c.is_whitespace() || c == ';')
            .next()
            .unwrap_or_default();
        if !in_string && blocks.contains(&keyword) {
            open_blocks += 1;
        } else if !in_string && keyword == "end" && !blocks.is_empty() {
            let Some(still_open) = open_blocks.checked_sub(1) else {
                return false;
            };
            open_blocks = still_open;
        }
        for c in line.chars() {
            match c {
                '"' => in_string = !in_string,
                _ if in_string || !is_flowchart => {}
                '[' | '(' | '{' => open_brackets.push(c),
                ']' | ')' | '}' => {
                    let open = match c {
                        ']' => '[',
                        ')' => '(',
                        _ => '{',
                    };
                    // Shapes like `A>label]` close what they didn't open.
                    if open_brackets.last() == Some(&open) {
                        open_brackets.pop();
                    }
                }
                _ => {}
            }
        }
        // A node's text only goes on to the next line in quotes.
        if !in_string && !open_brackets.is_empty() {
            return false;
        }
    }
    if in_string || open_blocks > 0 {
        return false;
    }
    let last = lines.last().copied().unwrap_or_default();
    let cut_after_arrow = ["--", "->", "==", "-.", "&", "|"]
        .iter()
        .any(|arrow| last.ends_with(arrow));
    !(is_flowchart && cut_after_arrow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_code_diagram() {
        let output = [
            "● mermaid",
            "  flowchart TB",
            "      subgraph Compile[\"编译期\"]",
            "          A --> B",
            "      end",
            "",
            "      A --> C",
            "  这张图展示了注册流程。",
        ];
        assert!(is_header(output[0]));
        assert_eq!(
            diagram_after(&output, 0, 80).as_deref(),
            Some(
                "flowchart TB\n    subgraph Compile[\"编译期\"]\n        A --> B\n    end\n\n    A --> C"
            )
        );
    }

    #[test]
    fn test_fenced_and_unindented_diagrams() {
        let fenced = ["```mermaid", "graph TD", "A --> B", "```", "done"];
        assert_eq!(
            diagram_after(&fenced, 0, 80).as_deref(),
            Some("graph TD\nA --> B")
        );
        let unindented = ["mermaid", "graph TD", "A --> B", "", "Some text"];
        assert_eq!(
            diagram_after(&unindented, 0, 80).as_deref(),
            Some("graph TD\nA --> B")
        );
        // Not a diagram.
        assert_eq!(diagram_after(&["mermaid", "is a tool"], 0, 80), None);
        assert_eq!(diagram_after(&["npm i mermaid", "graph TD"], 0, 80), None);
    }

    #[test]
    fn test_lines_wrapped_by_the_program() {
        // Claude Code wraps long lines itself, to the indentation of the block,
        // so the terminal doesn't mark them as wrapped.
        let output = [
            "● mermaid",
            "  flowchart TB",
            "      A[\"registers the class and its",
            "  methods\"] --> B",
            "      B --> C",
            "      C --> D[\"https://example.com/a/ver",
            "  y/long\"]",
            "",
            "  这张图展示了注册流程。",
        ];
        let diagram = diagram_after(&output, 0, 40);
        assert_eq!(
            diagram.as_deref(),
            Some(
                "flowchart TB\n    A[\"registers the class and its methods\"] --> B\n    B --> C\n    C --> D[\"https://example.com/a/very/long\"]"
            )
        );
        assert!(diagram.is_some_and(|diagram| is_complete(&diagram)));

        // The same lines in a wider terminal weren't wrapped: the diagram ends
        // at the first one that isn't indented.
        assert_eq!(
            diagram_after(&output, 0, 80).as_deref(),
            Some("flowchart TB\n    A[\"registers the class and its")
        );
    }

    #[test]
    fn test_overlays_left_out() {
        let output = [
            "● mermaid",
            "  flowchart TB",
            "      A --> B             Jump to bottom (ctrl+End) ↓",
            "      B --> C",
            "                    3 new messages (ctrl+End) ↓",
            "      C --> D",
            "",
            "  done",
        ];
        assert_eq!(
            diagram_after(&output, 0, 80).as_deref(),
            Some("flowchart TB\n    A --> B\n    B --> C\n    C --> D")
        );
        assert_eq!(
            without_overlay("      Jump to bottom: fn+↓ to scroll").as_deref(),
            None
        );
        assert_eq!(
            without_overlay("      A[\"Jump to bottom\"] --> B").as_deref(),
            Some("      A[\"Jump to bottom\"] --> B")
        );
    }

    #[test]
    fn test_incomplete_diagrams() {
        assert!(is_complete(
            "flowchart TB\n    subgraph Compile[\"编译期 (C++ 宏 / 模板)\"]\n        GDCLASS[\"GDCLASS(Class, Parent)<br/>object.h\"]\n        GDCLASS --> TYPEINFO\n    end\n    TYPEINFO --> CI"
        ));
        assert!(is_complete(
            "graph LR\n    A>asymmetric] --> B{decision}\n    B -->|yes| C((done))"
        ));
        assert!(is_complete(
            "sequenceDiagram\n    loop Every minute\n        A->>B: ping (still there?\n    end"
        ));
        assert!(is_complete("pie\n    \"Dogs\" : 386"));

        // A string or a node's text cut off.
        assert!(!is_complete("flowchart TB\n    A[\"编译期 --> B"));
        assert!(!is_complete("graph TD\n    A[开始 --> B\n    B --> C"));
        // A subgraph or a block of messages without its end, or an extra end.
        assert!(!is_complete(
            "flowchart TB\n    subgraph One\n        A --> B"
        ));
        assert!(!is_complete("flowchart TB\n    A --> B\n    end"));
        assert!(!is_complete(
            "sequenceDiagram\n    alt ok\n        A->>B: hi"
        ));
        // The last line cut off after an arrow.
        assert!(!is_complete("graph TD\n    A --> B\n    B -->"));
        assert!(!is_complete(""));

        // The bottom of the diagram off screen.
        let output = ["● mermaid", "  flowchart TB", "      subgraph DB[\"ClassDB"];
        let diagram = diagram_after(&output, 0, 80);
        assert_eq!(
            diagram.as_deref(),
            Some("flowchart TB\n    subgraph DB[\"ClassDB")
        );
        assert!(diagram.is_some_and(|diagram| !is_complete(&diagram)));
    }
}
