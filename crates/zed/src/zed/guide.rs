//! SOLO: the user guide on this computer. solo.exe carries the guide
//! (docs/guide), and on launch writes it as HTML pages to its data directory,
//! so help links open it in the browser without reaching GitHub.

use std::{fmt::Write as _, path::Path};

use anyhow::Result;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use util::ResultExt as _;

const GUIDES: &[(&str, &str, &str)] = &[
    ("en", "Solo User Guide", include_str!("../../../../docs/guide/en.md")),
    (
        "zh-CN",
        "Solo 用户指南",
        include_str!("../../../../docs/guide/zh-CN.md"),
    ),
];

/// Writes the guide's pages, in the background, and points the help links at
/// them once they're written.
pub fn write_local_guide() {
    std::thread::spawn(|| {
        let dir = paths::data_dir().join("guide");
        write_pages(&dir).log_err();
    });
}

fn write_pages(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, title, markdown) in GUIDES {
        let path = dir.join(format!("{name}.html"));
        let html = render_page(title, markdown);
        // Unchanged since the last launch: leave the file alone.
        if std::fs::read_to_string(&path).ok().as_deref() != Some(html.as_str()) {
            std::fs::write(&path, html)?;
        }
    }
    let url = |name: &str| {
        url::Url::from_file_path(dir.join(format!("{name}.html")))
            .map(|url| url.to_string())
            .map_err(|()| anyhow::anyhow!("no file URL for {}", dir.display()))
    };
    util::i18n::set_local_guide(url("en")?, url("zh-CN")?);
    Ok(())
}

fn render_page(title: &str, markdown: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{STYLE}</style>
</head>
<body>
<main>
{body}</main>
</body>
</html>
"#,
        title = escape(title),
        body = render_markdown(markdown),
    )
}

const STYLE: &str = r#"
:root { color-scheme: light dark; --fg: #1f2328; --muted: #59636e; --bg: #ffffff; --line: #d1d9e0; --code: #f6f8fa; --link: #0969da; }
@media (prefers-color-scheme: dark) { :root { --fg: #e6edf3; --muted: #9198a1; --bg: #0d1117; --line: #3d444d; --code: #161b22; --link: #4493f8; } }
body { margin: 0; background: var(--bg); color: var(--fg); font: 16px/1.6 -apple-system, "Segoe UI", "Microsoft YaHei", sans-serif; }
main { max-width: 860px; margin: 0 auto; padding: 32px 16px 64px; }
h1, h2 { border-bottom: 1px solid var(--line); padding-bottom: .3em; }
a { color: var(--link); }
code { font-family: Consolas, "Cascadia Mono", monospace; background: var(--code); padding: .15em .35em; border-radius: 4px; font-size: 90%; }
pre { background: var(--code); padding: 12px 16px; border-radius: 6px; overflow-x: auto; }
pre code { padding: 0; background: none; }
blockquote { margin: 0; padding: 0 1em; color: var(--muted); border-left: 4px solid var(--line); }
table { border-collapse: collapse; display: block; overflow-x: auto; }
th, td { border: 1px solid var(--line); padding: 6px 12px; }
"#;

/// The guide's Markdown as HTML: what pulldown-cmark parses, with GitHub's ids
/// on the headings and links between the guide's pages pointing at their
/// HTML.
fn render_markdown(markdown: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut html = String::new();
    // Where the open heading's tag goes once its text, and so its id, is
    // known, and that text.
    let mut heading: Option<(usize, String)> = None;
    let mut in_table_head = false;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => html.push_str("<p>"),
                Tag::Heading { .. } => heading = Some((html.len(), String::new())),
                Tag::BlockQuote(_) => html.push_str("<blockquote>\n"),
                Tag::CodeBlock(kind) => match kind {
                    CodeBlockKind::Fenced(language) if !language.is_empty() => {
                        write!(html, "<pre><code class=\"language-{}\">", escape(&language))
                            .ok();
                    }
                    _ => html.push_str("<pre><code>"),
                },
                Tag::List(Some(1)) => html.push_str("<ol>\n"),
                Tag::List(Some(start)) => {
                    write!(html, "<ol start=\"{start}\">\n").ok();
                }
                Tag::List(None) => html.push_str("<ul>\n"),
                Tag::Item => html.push_str("<li>"),
                Tag::Table(_) => html.push_str("<table>\n"),
                Tag::TableHead => {
                    in_table_head = true;
                    html.push_str("<thead><tr>");
                }
                Tag::TableRow => html.push_str("<tr>"),
                Tag::TableCell => html.push_str(if in_table_head { "<th>" } else { "<td>" }),
                Tag::Emphasis => html.push_str("<em>"),
                Tag::Strong => html.push_str("<strong>"),
                Tag::Strikethrough => html.push_str("<del>"),
                Tag::Link {
                    dest_url, title, ..
                } => {
                    write!(html, "<a href=\"{}\"", escape(&local_link(&dest_url))).ok();
                    if !title.is_empty() {
                        write!(html, " title=\"{}\"", escape(&title)).ok();
                    }
                    html.push('>');
                }
                Tag::Image { dest_url, .. } => {
                    write!(html, "<img src=\"{}\" alt=\"", escape(&dest_url)).ok();
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => html.push_str("</p>\n"),
                TagEnd::Heading(level) => {
                    if let Some((start, text)) = heading.take() {
                        let level = heading_level(level);
                        html.insert_str(
                            start,
                            &format!("<h{level} id=\"{}\">", escape(&heading_id(&text))),
                        );
                        write!(html, "</h{level}>\n").ok();
                    }
                }
                TagEnd::BlockQuote(_) => html.push_str("</blockquote>\n"),
                TagEnd::CodeBlock => html.push_str("</code></pre>\n"),
                TagEnd::List(true) => html.push_str("</ol>\n"),
                TagEnd::List(false) => html.push_str("</ul>\n"),
                TagEnd::Item => html.push_str("</li>\n"),
                TagEnd::Table => html.push_str("</tbody></table>\n"),
                TagEnd::TableHead => {
                    in_table_head = false;
                    html.push_str("</tr></thead><tbody>\n");
                }
                TagEnd::TableRow => html.push_str("</tr>\n"),
                TagEnd::TableCell => html.push_str(if in_table_head { "</th>" } else { "</td>" }),
                TagEnd::Emphasis => html.push_str("</em>"),
                TagEnd::Strong => html.push_str("</strong>"),
                TagEnd::Strikethrough => html.push_str("</del>"),
                TagEnd::Link => html.push_str("</a>"),
                TagEnd::Image => html.push_str("\">"),
                _ => {}
            },
            Event::Text(text) => {
                if let Some((_, heading_text)) = &mut heading {
                    heading_text.push_str(&text);
                }
                html.push_str(&escape(&text));
            }
            Event::Code(code) => {
                if let Some((_, heading_text)) = &mut heading {
                    heading_text.push_str(&code);
                }
                write!(html, "<code>{}</code>", escape(&code)).ok();
            }
            // The guide has no HTML of its own; shown as written.
            Event::Html(text) | Event::InlineHtml(text) => html.push_str(&escape(&text)),
            Event::SoftBreak => html.push('\n'),
            Event::HardBreak => html.push_str("<br>\n"),
            Event::Rule => html.push_str("<hr>\n"),
            Event::TaskListMarker(checked) => html.push_str(if checked {
                "<input type=\"checkbox\" checked disabled> "
            } else {
                "<input type=\"checkbox\" disabled> "
            }),
            _ => {}
        }
    }
    html
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The id GitHub gives a heading, so `#...` links work as they do there.
fn heading_id(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

/// A link to another of the guide's pages (`en.md`, `zh-CN.md#...`) points
/// at its HTML; any other link is left as it is.
/// Where the documents next to the guide's are, for links to them.
const REPOSITORY_DOCS: &str = "https://github.com/lucas9801/solo/blob/main/docs";

fn local_link(url: &str) -> String {
    let (path, fragment) = url.split_once('#').map_or((url, None), |(p, f)| (p, Some(f)));
    let is_guide_page = GUIDES
        .iter()
        .any(|(name, _, _)| path == format!("{name}.md"));
    if !is_guide_page {
        // Other documents of the repository aren't copied with the guide:
        // they open on GitHub.
        let is_relative = !url.contains("://") && !url.starts_with('#');
        if is_relative && let Some(document) = path.strip_prefix("../") {
            return match fragment {
                Some(fragment) => format!("{REPOSITORY_DOCS}/{document}#{fragment}"),
                None => format!("{REPOSITORY_DOCS}/{document}"),
            };
        }
        return url.to_string();
    }
    let page = path.replace(".md", ".html");
    match fragment {
        Some(fragment) => format!("{page}#{fragment}"),
        None => page,
    }
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renders_the_guide() {
        let html = render_markdown(
            "# Solo 用户指南\n\n[English](en.md)\n\n## 2. Install `solo`\n\n| a | b |\n|---|---|\n| <x> | y |\n\n- one\n- two\n\n```sh\nsolo --help\n```\n",
        );
        assert!(html.contains("<h1 id=\"solo-用户指南\">Solo 用户指南</h1>"), "{html}");
        assert!(html.contains("<a href=\"en.html\">English</a>"), "{html}");
        assert!(
            html.contains("<h2 id=\"2-install-solo\">2. Install <code>solo</code></h2>"),
            "{html}"
        );
        assert!(html.contains("<thead><tr><th>a</th><th>b</th></tr></thead>"), "{html}");
        assert!(html.contains("<td>&lt;x&gt;</td>"), "{html}");
        assert!(html.contains("<ul>\n<li>one</li>\n<li>two</li>\n</ul>"), "{html}");
        assert!(
            html.contains("<pre><code class=\"language-sh\">solo --help\n</code></pre>"),
            "{html}"
        );
    }

    #[test]
    fn test_links_between_pages() {
        assert_eq!(local_link("zh-CN.md#3-终端"), "zh-CN.html#3-终端");
        assert_eq!(local_link("https://github.com/a.md"), "https://github.com/a.md");
        assert_eq!(
            local_link("../agent-cli.md"),
            "https://github.com/lucas9801/solo/blob/main/docs/agent-cli.md"
        );
    }

    #[test]
    fn test_writes_the_pages() {
        let dir = tempfile::tempdir().unwrap();
        write_pages(dir.path()).unwrap();
        for name in ["en", "zh-CN"] {
            let html = std::fs::read_to_string(dir.path().join(format!("{name}.html"))).unwrap();
            assert!(html.starts_with("<!doctype html>"));
            assert!(html.contains("</main>"));
        }
        assert!(util::i18n::guide_url().starts_with("file:///"));
    }
}
