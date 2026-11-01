//! SOLO: Mermaid diagrams printed into the terminal, such as one an agent
//! drew in its answer, to be shown rendered.
//!
//! A diagram starts below a `mermaid` line: a code fence (```` ```mermaid ````)
//! or, as Claude Code draws code blocks, the bare language name.

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

/// The diagram whose header is `lines[header_ix]`, without the indentation it
/// was printed with.
pub(crate) fn diagram_after(lines: &[&str], header_ix: usize) -> Option<String> {
    let fenced = header(lines.get(header_ix)?)?;
    let body = &lines[header_ix + 1..];
    let first = body.iter().position(|line| !line.trim().is_empty())?;
    let base = indent(body[first]);
    let start = body[first].trim_start();
    if !DIAGRAM_TYPES.iter().any(|kind| start.starts_with(kind)) {
        return None;
    }
    let rest = first + 1;
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
        if body[rest..shallower].iter().any(|line| !line.trim().is_empty()) {
            shallower
        } else {
            body[rest..]
                .iter()
                .position(|line| line.trim().is_empty())
                .map_or(body.len(), |ix| rest + ix)
        }
    };
    let mut diagram: Vec<&str> = body[first..end]
        .iter()
        .map(|line| &line[indent(line).min(base)..])
        .collect();
    while diagram.last().is_some_and(|line| line.trim().is_empty()) {
        diagram.pop();
    }
    Some(diagram.join("\n"))
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
            diagram_after(&output, 0).as_deref(),
            Some("flowchart TB\n    subgraph Compile[\"编译期\"]\n        A --> B\n    end\n\n    A --> C")
        );
    }

    #[test]
    fn test_fenced_and_unindented_diagrams() {
        let fenced = ["```mermaid", "graph TD", "A --> B", "```", "done"];
        assert_eq!(
            diagram_after(&fenced, 0).as_deref(),
            Some("graph TD\nA --> B")
        );
        let unindented = ["mermaid", "graph TD", "A --> B", "", "Some text"];
        assert_eq!(
            diagram_after(&unindented, 0).as_deref(),
            Some("graph TD\nA --> B")
        );
        // Not a diagram.
        assert_eq!(diagram_after(&["mermaid", "is a tool"], 0), None);
        assert_eq!(diagram_after(&["npm i mermaid", "graph TD"], 0), None);
    }
}
