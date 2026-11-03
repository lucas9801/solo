//! Highlighting for PowerShell commands in the input. Zed has no PowerShell
//! grammar, and a command line needs little more than a tokenizer: commands,
//! parameters, strings, variables, operators and comments.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Command,
    Keyword,
    Parameter,
    String,
    Variable,
    Number,
    Operator,
    Type,
    Comment,
}

impl TokenKind {
    pub const ALL: [TokenKind; 9] = [
        TokenKind::Command,
        TokenKind::Keyword,
        TokenKind::Parameter,
        TokenKind::String,
        TokenKind::Variable,
        TokenKind::Number,
        TokenKind::Operator,
        TokenKind::Type,
        TokenKind::Comment,
    ];

    /// The theme's syntax style used for this kind.
    pub fn theme_name(self) -> &'static str {
        match self {
            TokenKind::Command => "function",
            TokenKind::Keyword => "keyword",
            TokenKind::Parameter => "property",
            TokenKind::String => "string",
            TokenKind::Variable => "variable.special",
            TokenKind::Number => "number",
            TokenKind::Operator => "operator",
            TokenKind::Type => "type",
            TokenKind::Comment => "comment",
        }
    }
}

const KEYWORDS: &[&str] = &[
    "begin", "break", "catch", "class", "continue", "data", "do", "dynamicparam", "else",
    "elseif", "end", "enum", "exit", "filter", "finally", "for", "foreach", "function", "if",
    "in", "param", "process", "return", "switch", "throw", "trap", "try", "until", "using",
    "while",
];

const OPERATORS: &[&str] = &[
    "and", "or", "not", "xor", "band", "bor", "bnot", "bxor", "shl", "shr", "eq", "ne", "gt",
    "ge", "lt", "le", "like", "notlike", "match", "notmatch", "contains", "notcontains", "in",
    "notin", "replace", "split", "join", "is", "isnot", "as", "f", "ieq", "ine", "ceq", "cne",
    "ilike", "clike", "imatch", "cmatch",
];

/// Tokens of a PowerShell command line, in order. Text between them (spaces,
/// plain arguments) is left unstyled.
pub fn powershell_tokens(text: &str) -> Vec<(Range<usize>, TokenKind)> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut ix = 0;
    // Whether the next word is in command position.
    let mut expect_command = true;
    while ix < bytes.len() {
        let c = bytes[ix];
        let start = ix;
        match c {
            b' ' | b'\t' | b'\r' => ix += 1,
            b'\n' => {
                ix += 1;
                expect_command = true;
            }
            b'#' => {
                ix = text[ix..].find('\n').map_or(bytes.len(), |len| ix + len);
                tokens.push((start..ix, TokenKind::Comment));
            }
            b'<' if bytes.get(ix + 1) == Some(&b'#') => {
                ix = text[ix + 2..].find("#>").map_or(bytes.len(), |len| ix + 2 + len + 2);
                tokens.push((start..ix, TokenKind::Comment));
            }
            b'\'' | b'"' => {
                ix = string_end(bytes, ix);
                tokens.push((start..ix, TokenKind::String));
                expect_command = false;
            }
            b'@' if matches!(bytes.get(ix + 1), Some(b'\'' | b'"')) => {
                // Here-strings run to a line starting with the closing quote.
                let quote = bytes[ix + 1] as char;
                let close = format!("\n{quote}@");
                ix = text[ix + 2..]
                    .find(&close)
                    .map_or(bytes.len(), |len| ix + 2 + len + close.len());
                tokens.push((start..ix, TokenKind::String));
                expect_command = false;
            }
            b'$' => {
                ix = variable_end(text, ix);
                tokens.push((start..ix, TokenKind::Variable));
                expect_command = false;
            }
            b'|' | b';' | b'&' => {
                ix += 1;
                if c != b';' && bytes.get(ix) == Some(&c) {
                    ix += 1;
                }
                tokens.push((start..ix, TokenKind::Operator));
                expect_command = true;
            }
            b'(' | b'{' => {
                ix += 1;
                expect_command = true;
            }
            b')' | b'}' | b',' => {
                ix += 1;
                expect_command = false;
            }
            b'>' | b'=' => {
                // Redirections like `2>&1` start with a digit and are handled
                // as words; these are the bare operators.
                ix += 1;
                while ix < bytes.len() && matches!(bytes[ix], b'>' | b'=' | b'&') {
                    ix += 1;
                }
                if bytes[ix - 1] == b'&' && bytes.get(ix).is_some_and(u8::is_ascii_digit) {
                    ix += 1;
                }
                tokens.push((start..ix, TokenKind::Operator));
                expect_command = c == b'=';
            }
            b'[' if is_type_literal(text, ix) => {
                ix += text[ix..].find(']').map_or(bytes.len() - ix, |len| len + 1);
                tokens.push((start..ix, TokenKind::Type));
            }
            _ => {
                ix = word_end(bytes, ix);
                let word = &text[start..ix];
                let kind = classify_word(word, expect_command);
                if let Some(kind) = kind {
                    tokens.push((start..ix, kind));
                }
                expect_command = match kind {
                    Some(TokenKind::Keyword) => !matches!(
                        word.to_ascii_lowercase().as_str(),
                        "function" | "filter" | "class" | "enum"
                    ),
                    Some(TokenKind::Operator) => !is_redirection(word),
                    _ => false,
                };
            }
        }
    }
    tokens
}

fn classify_word(word: &str, expect_command: bool) -> Option<TokenKind> {
    let lower = word.to_ascii_lowercase();
    if is_number(lower.strip_prefix('-').unwrap_or(&lower)) {
        return Some(TokenKind::Number);
    }
    if expect_command && KEYWORDS.contains(&lower.as_str()) {
        return Some(TokenKind::Keyword);
    }
    if let Some(name) = lower.strip_prefix('-')
        && !name.is_empty()
        && !expect_command
    {
        if OPERATORS.contains(&name) {
            return Some(TokenKind::Operator);
        }
        return Some(TokenKind::Parameter);
    }
    if is_redirection(word) {
        return Some(TokenKind::Operator);
    }
    if expect_command {
        return Some(TokenKind::Command);
    }
    None
}

/// Whether a lowercase word is a number, like `42`, `1.5`, `0xff` or `10mb`.
fn is_number(lower: &str) -> bool {
    if let Some(hex) = lower.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    let digits = lower
        .strip_suffix('b')
        .map_or(lower, |rest| rest.trim_end_matches(['k', 'm', 'g', 't', 'p']))
        .trim_end_matches(['l', 'd']);
    !digits.is_empty()
        && digits.as_bytes()[0].is_ascii_digit()
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn is_redirection(word: &str) -> bool {
    let rest = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '*');
    rest.len() < word.len() && rest.starts_with('>')
}

fn is_type_literal(text: &str, ix: usize) -> bool {
    let Some(len) = text[ix + 1..].find(']') else {
        return false;
    };
    let name = &text[ix + 1..ix + 1 + len];
    !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '[' | ',' | ' '))
}

fn word_end(bytes: &[u8], mut ix: usize) -> usize {
    while ix < bytes.len() {
        match bytes[ix] {
            b' ' | b'\t' | b'\r' | b'\n' | b'|' | b';' | b'(' | b')' | b'{' | b'}' | b','
            | b'\'' | b'"' => break,
            b'$' if ix > 0 => break,
            b'`' => ix += 2,
            _ => ix += 1,
        }
    }
    ix.min(bytes.len())
}

fn string_end(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut ix = start + 1;
    while ix < bytes.len() {
        let c = bytes[ix];
        if c == b'`' && quote == b'"' {
            ix += 2;
            continue;
        }
        ix += 1;
        if c == quote {
            // A doubled quote is an escaped one.
            if bytes.get(ix) == Some(&quote) {
                ix += 1;
                continue;
            }
            return ix;
        }
    }
    bytes.len()
}

fn variable_end(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let ix = start + 1;
    match bytes.get(ix) {
        Some(b'{') => text[ix..].find('}').map_or(bytes.len(), |len| ix + len + 1),
        Some(b'$' | b'?' | b'^' | b'_') if !bytes.get(ix + 1).is_some_and(is_name_byte) => ix + 1,
        _ => {
            let mut end = ix;
            while end < bytes.len() && (is_name_byte(&bytes[end]) || bytes[end] == b':') {
                // `$env:Path` is one variable, but `$a::b` is not.
                if bytes[end] == b':' && !bytes.get(end + 1).is_some_and(is_name_byte) {
                    break;
                }
                end += 1;
            }
            end
        }
    }
}

fn is_name_byte(byte: &u8) -> bool {
    byte.is_ascii_alphanumeric() || *byte == b'_' || *byte >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&str, TokenKind)> {
        powershell_tokens(text)
            .into_iter()
            .map(|(range, kind)| (&text[range], kind))
            .collect()
    }

    #[test]
    fn highlights_a_pipeline() {
        use TokenKind::*;
        assert_eq!(
            kinds("Get-ChildItem -Recurse *.rs | Where-Object { $_.Length -gt 1kb } # big"),
            [
                ("Get-ChildItem", Command),
                ("-Recurse", Parameter),
                ("|", Operator),
                ("Where-Object", Command),
                ("$_", Variable),
                ("-gt", Operator),
                ("1kb", Number),
                ("# big", Comment),
            ]
        );
    }

    #[test]
    fn highlights_strings_variables_and_keywords() {
        use TokenKind::*;
        assert_eq!(
            kinds("$env:Path = 'it''s'; if ($x) { git commit -m \"a `\" b\" } 2>&1"),
            [
                ("$env:Path", Variable),
                ("=", Operator),
                ("'it''s'", String),
                (";", Operator),
                ("if", Keyword),
                ("$x", Variable),
                ("git", Command),
                ("-m", Parameter),
                ("\"a `\" b\"", String),
                ("2>&1", Operator),
            ]
        );
        assert_eq!(
            kinds("[int]$n = -5"),
            [("[int]", Type), ("$n", Variable), ("=", Operator), ("-5", Number)]
        );
    }

    #[test]
    fn tolerates_unfinished_input() {
        use TokenKind::*;
        assert_eq!(kinds("echo 'abc"), [("echo", Command), ("'abc", String)]);
        assert_eq!(kinds("$"), [("$", Variable)]);
        assert_eq!(kinds("<# open"), [("<# open", Comment)]);
        assert_eq!(kinds("cd ä"), [("cd", Command)]);
    }
}
