//! Picking elements on a page to comment on: the selector for a picked element,
//! and what the page reports about it.

use serde::Deserialize;

/// One element of the page's DOM, as the pick script reports it: enough to
/// build a selector and check that it matches only the picked element.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct DomNode {
    #[serde(rename = "t")]
    pub tag: String,
    #[serde(rename = "i", default)]
    pub id: Option<String>,
    #[serde(rename = "d", default)]
    pub test_id: Option<String>,
    #[serde(rename = "c", default)]
    pub classes: Vec<String>,
    #[serde(rename = "k", default)]
    pub children: Vec<DomNode>,
}

/// A selector made of compound selectors joined by `>`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    test_id: Option<String>,
    classes: Vec<String>,
    nth_child: Option<usize>,
}

impl Compound {
    fn matches(&self, node: &DomNode, index_in_parent: usize) -> bool {
        self.tag.as_ref().is_none_or(|tag| *tag == node.tag)
            && self
                .id
                .as_ref()
                .is_none_or(|id| node.id.as_ref() == Some(id))
            && self
                .test_id
                .as_ref()
                .is_none_or(|test_id| node.test_id.as_ref() == Some(test_id))
            && self
                .classes
                .iter()
                .all(|class| node.classes.contains(class))
            && self.nth_child.is_none_or(|nth| nth == index_in_parent + 1)
    }

    fn to_css(&self) -> String {
        let mut css = self.tag.clone().unwrap_or_default();
        if let Some(id) = &self.id {
            css.push('#');
            css.push_str(&css_escape(id));
        }
        if let Some(test_id) = &self.test_id {
            css.push_str(&format!(
                "[data-testid=\"{}\"]",
                test_id.replace('"', "\\\"")
            ));
        }
        for class in &self.classes {
            css.push('.');
            css.push_str(&css_escape(class));
        }
        if let Some(nth) = self.nth_child {
            css.push_str(&format!(":nth-child({nth})"));
        }
        if css.is_empty() { "*".to_string() } else { css }
    }
}

fn css_escape(identifier: &str) -> String {
    let mut escaped = String::new();
    for (index, character) in identifier.chars().enumerate() {
        let plain = character.is_ascii_alphanumeric()
            || character == '-'
            || character == '_'
            || !character.is_ascii();
        if plain && !(index == 0 && character.is_ascii_digit()) {
            escaped.push(character);
        } else if character.is_ascii_digit() {
            escaped.push_str(&format!("\\{:x} ", character as u32));
        } else {
            escaped.push('\\');
            escaped.push(character);
        }
    }
    escaped
}

/// Counts the elements of `root`'s tree that `chain` (outermost first, joined
/// by child combinators) matches.
fn count_matches(root: &DomNode, chain: &[Compound]) -> usize {
    fn walk<'a>(
        node: &'a DomNode,
        index: usize,
        ancestors: &mut Vec<(&'a DomNode, usize)>,
        chain: &[Compound],
        count: &mut usize,
    ) {
        if chain_matches(node, index, ancestors, chain) {
            *count += 1;
        }
        ancestors.push((node, index));
        for (child_index, child) in node.children.iter().enumerate() {
            walk(child, child_index, ancestors, chain, count);
        }
        ancestors.pop();
    }
    let mut count = 0;
    walk(root, 0, &mut Vec::new(), chain, &mut count);
    count
}

fn chain_matches(
    node: &DomNode,
    index: usize,
    ancestors: &[(&DomNode, usize)],
    chain: &[Compound],
) -> bool {
    let Some((last, rest)) = chain.split_last() else {
        return false;
    };
    if !last.matches(node, index) {
        return false;
    }
    match ancestors.split_last() {
        _ if rest.is_empty() => true,
        Some(((parent, parent_index), above)) => chain_matches(parent, *parent_index, above, rest),
        None => false,
    }
}

/// Classes worth naming in a selector: not generated-looking ones (CSS modules,
/// styled-components hashes) and not Tailwind's utility syntax.
fn useful_class(class: &str) -> bool {
    !class.is_empty()
        && class.len() <= 32
        && !class.contains([':', '[', ']', '/', '(', ')', '.', '%', '!'])
        && !(class.len() >= 6
            && class.chars().filter(|c| c.is_ascii_digit()).count() * 3 >= class.len())
}

/// The plain compound for a node: the tag plus up to `max_classes` classes.
fn simple_compound(node: &DomNode, max_classes: usize) -> Compound {
    Compound {
        tag: Some(node.tag.clone()),
        classes: node
            .classes
            .iter()
            .filter(|class| useful_class(class))
            .take(max_classes)
            .cloned()
            .collect(),
        ..Default::default()
    }
}

/// A short selector that matches only the element at `path` (child indices
/// from `root`): its id, its `data-testid`, its tag and classes, and failing
/// those, a chain of parents with `:nth-child`.
pub fn unique_selector(root: &DomNode, path: &[usize]) -> Option<String> {
    let mut nodes = vec![(root, 0usize)];
    for &index in path {
        let parent = nodes.last()?.0;
        nodes.push((parent.children.get(index)?, index));
    }
    let chain = selector_chain(root, &nodes)?;
    Some(
        chain
            .iter()
            .map(Compound::to_css)
            .collect::<Vec<_>>()
            .join(" > "),
    )
}

fn selector_chain(root: &DomNode, nodes: &[(&DomNode, usize)]) -> Option<Vec<Compound>> {
    let (node, index) = *nodes.last()?;
    let unique = |chain: &[Compound]| count_matches(root, chain) == 1;

    if let Some(id) = node.id.as_ref().filter(|id| !id.is_empty()) {
        let compound = Compound {
            id: Some(id.clone()),
            ..Default::default()
        };
        if unique(std::slice::from_ref(&compound)) {
            return Some(vec![compound]);
        }
    }
    if let Some(test_id) = node.test_id.as_ref().filter(|id| !id.is_empty()) {
        let compound = Compound {
            test_id: Some(test_id.clone()),
            ..Default::default()
        };
        if unique(std::slice::from_ref(&compound)) {
            return Some(vec![compound]);
        }
    }
    for max_classes in 0..=3 {
        let compound = simple_compound(node, max_classes);
        if max_classes > 0 && compound.classes.len() < max_classes {
            break;
        }
        if unique(std::slice::from_ref(&compound)) {
            return Some(vec![compound]);
        }
    }

    // Not unique alone: under its parent's selector, by class, then by position.
    if nodes.len() < 2 {
        return Some(vec![simple_compound(node, 0)]);
    }
    let parent_chain = selector_chain(root, &nodes[..nodes.len() - 1])?;
    let mut compound = simple_compound(node, 2);
    let mut chain = parent_chain.clone();
    chain.push(compound.clone());
    if unique(&chain) {
        return Some(chain);
    }
    compound.nth_child = Some(index + 1);
    compound.classes.clear();
    let mut chain = parent_chain;
    chain.push(compound);
    Some(chain)
}

/// What the pick script reports about a picked element.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct PickedElement {
    /// Child indices from `<html>` to the element.
    pub path: Vec<usize>,
    /// The element's visible text, collapsed.
    pub text: String,
    pub html: String,
    /// `name value` pairs of computed styles worth telling the agent.
    pub styles: Vec<(String, String)>,
    /// The element's box in CSS pixels, relative to the viewport.
    pub rect: PickRect,
    /// How far the page is scrolled, to turn `rect` into document coordinates.
    #[serde(rename = "scrollX", default)]
    pub scroll_x: f64,
    #[serde(rename = "scrollY", default)]
    pub scroll_y: f64,
    pub document: DomNode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub struct PickRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// HTML longer than this is cut, with an ellipsis.
pub const MAX_HTML_LEN: usize = 1024;

pub fn truncate_html(html: &str) -> String {
    let html = html.trim();
    if html.len() <= MAX_HTML_LEN {
        return html.to_string();
    }
    let mut end = MAX_HTML_LEN;
    while !html.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &html[..end])
}

/// The first `count` characters of `text` with whitespace collapsed.
pub fn short_text(text: &str, count: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = collapsed.chars().take(count).collect();
    if collapsed.chars().count() > count {
        short.push('…');
    }
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(tag: &str) -> DomNode {
        DomNode {
            tag: tag.into(),
            ..Default::default()
        }
    }

    fn with_classes(tag: &str, classes: &[&str]) -> DomNode {
        DomNode {
            tag: tag.into(),
            classes: classes.iter().map(|class| class.to_string()).collect(),
            ..Default::default()
        }
    }

    fn page(body_children: Vec<DomNode>) -> DomNode {
        DomNode {
            tag: "html".into(),
            children: vec![
                node("head"),
                DomNode {
                    tag: "body".into(),
                    children: body_children,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn prefers_a_unique_id() {
        let mut button = with_classes("button", &["save"]);
        button.id = Some("save-button".into());
        let root = page(vec![button, with_classes("button", &["save"])]);
        assert_eq!(
            unique_selector(&root, &[1, 0]).as_deref(),
            Some("#save-button")
        );
    }

    #[test]
    fn uses_test_id_when_there_is_no_id() {
        let mut field = node("input");
        field.test_id = Some("email".into());
        let root = page(vec![node("input"), field]);
        assert_eq!(
            unique_selector(&root, &[1, 1]).as_deref(),
            Some("[data-testid=\"email\"]")
        );
    }

    #[test]
    fn uses_tag_and_classes() {
        let root = page(vec![
            with_classes("h2", &["title"]),
            with_classes("button", &["btn"]),
            with_classes("button", &["btn", "save"]),
        ]);
        assert_eq!(unique_selector(&root, &[1, 0]).as_deref(), Some("h2"));
        assert_eq!(
            unique_selector(&root, &[1, 2]).as_deref(),
            Some("button.btn.save")
        );
    }

    #[test]
    fn falls_back_to_nth_child() {
        let list = DomNode {
            tag: "ul".into(),
            classes: vec!["menu".into()],
            children: vec![node("li"), node("li"), node("li")],
            ..Default::default()
        };
        let root = page(vec![list, with_classes("ul", &["other"])]);
        assert_eq!(
            unique_selector(&root, &[1, 0, 1]).as_deref(),
            Some("ul.menu > li:nth-child(2)")
        );
    }

    #[test]
    fn duplicated_ids_and_generated_classes_are_skipped() {
        let mut first = with_classes("div", &["css-1x2y3z4", "card"]);
        first.id = Some("dup".into());
        let mut second = with_classes("div", &["css-9a8b7c6"]);
        second.id = Some("dup".into());
        let root = page(vec![first, second]);
        assert_eq!(unique_selector(&root, &[1, 0]).as_deref(), Some("div.card"));
    }

    #[test]
    fn escapes_identifiers() {
        let mut item = node("div");
        item.id = Some("1st:item".into());
        let root = page(vec![item]);
        assert_eq!(
            unique_selector(&root, &[1, 0]).as_deref(),
            Some("#\\31 st\\:item")
        );
    }

    #[test]
    fn html_is_cut_at_about_a_kilobyte() {
        assert_eq!(truncate_html(" <b>hi</b> "), "<b>hi</b>");
        let long = format!("<p>{}</p>", "字".repeat(600));
        let cut = truncate_html(&long);
        assert!(cut.ends_with('…'));
        assert!(cut.len() <= MAX_HTML_LEN + '…'.len_utf8());
    }

    #[test]
    fn short_text_collapses_whitespace() {
        assert_eq!(short_text("  账户\n  设置 ", 20), "账户 设置");
        assert_eq!(short_text("abcdefghij", 4), "abcd…");
    }
}
