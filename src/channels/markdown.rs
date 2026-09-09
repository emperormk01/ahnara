//! Markdown formatting for Telegram
//!
//! Telegram's MarkdownV2 requires escaping: _ * [ ] ( ) ~ ` > # + - = | { } . !
//! This module converts common markdown to Telegram-compatible MarkdownV2.

/// Convert common markdown to Telegram MarkdownV2 format.
pub fn markdown_to_telegram(text: &str) -> String {
    let raw = convert_inner(text);
    if markers_are_balanced(&raw) {
        raw
    } else {
        // Strip all formatting markers and return escaped plain text
        escape_markdown_v2(text)
    }
}

fn markers_are_balanced(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for marker in ['_', '*', '~'] {
        if !count_marker_occurrences(&chars, marker) {
            return false;
        }
    }
    true
}

fn count_marker_occurrences(chars: &[char], marker: char) -> bool {
    let mut count = 0;
    let mut j = 0;
    while j < chars.len() {
        j = skip_special_region(chars, j);
        if j < chars.len() && chars[j] == marker {
            count += 1;
        }
        j += 1;
    }
    count % 2 == 0
}

fn skip_special_region(chars: &[char], j: usize) -> usize {
    if j >= chars.len() {
        return j;
    }
    if chars[j] == '\\' && j + 1 < chars.len() {
        return j + 2;
    }
    if chars[j] == '`' && j + 2 < chars.len() && chars[j+1] == '`' && chars[j+2] == '`' {
        return skip_triple_backtick(chars, j + 3);
    }
    if chars[j] == '`' {
        return skip_inline_code(chars, j + 1);
    }
    if chars[j] == '[' {
        return skip_link_structure(chars, j);
    }
    j
}

fn skip_triple_backtick(chars: &[char], mut j: usize) -> usize {
    while j + 2 < chars.len() && !(chars[j] == '`' && chars[j+1] == '`' && chars[j+2] == '`') {
        j += 1;
    }
    if j + 2 < chars.len() { j += 3; }
    j
}

fn skip_inline_code(chars: &[char], mut j: usize) -> usize {
    while j < chars.len() && chars[j] != '`' {
        j += 1;
    }
    if j < chars.len() { j += 1; }
    j
}

fn skip_link_structure(chars: &[char], mut j: usize) -> usize {
    j += 1;
    while j < chars.len() && chars[j] != ']' {
        j += 1;
    }
    if j < chars.len() { j += 1; }
    if j < chars.len() && chars[j] == '(' {
        j += 1;
        let mut paren_depth = 1;
        while j < chars.len() && paren_depth > 0 {
            if chars[j] == '\\' && j + 1 < chars.len() {
                j += 2;
                continue;
            }
            if chars[j] == '(' { paren_depth += 1; }
            if chars[j] == ')' { paren_depth -= 1; }
            if paren_depth > 0 { j += 1; }
        }
        if j < chars.len() { j += 1; }
    }
    j
}

fn convert_inner(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut i = 0;

    while i < text.len() {
        let rest = &text[i..];

        if rest.starts_with("```") {
            if let Some(close_rel) = rest[3..].find("```") {
                let inner = &rest[3..3 + close_rel];
                out.push_str("```");
                out.push_str(&escape_code(inner));
                out.push_str("```");
                i += 3 + close_rel + 3;
                continue;
            }
        }

        if rest.starts_with('`') {
            if let Some(close_rel) = rest[1..].find('`') {
                let inner = &rest[1..1 + close_rel];
                out.push('`');
                out.push_str(&escape_code(inner));
                out.push('`');
                i += 1 + close_rel + 1;
                continue;
            }
        }

        if let Some((marker, telegram_marker, consumed)) = parse_marker(rest) {
            if let Some(close_rel) = rest[consumed..].find(marker) {
                let inner_end = consumed + close_rel;
                let inner = &rest[consumed..inner_end];
                if !inner.trim().is_empty() {
                    out.push_str(telegram_marker);
                    out.push_str(&markdown_to_telegram(inner));
                    out.push_str(telegram_marker);
                    i += inner_end + marker.len();
                    continue;
                }
            }
        }

        if rest.starts_with('[') {
            if let Some((link, consumed)) = parse_link(rest) {
                out.push_str(&link);
                i += consumed;
                continue;
            }
        }

        let ch = rest.chars().next().expect("non-empty string slice");
        push_escaped_char(&mut out, ch);
        i += ch.len_utf8();
    }

    out
}

fn parse_marker(rest: &str) -> Option<(&'static str, &'static str, usize)> {
    if rest.starts_with("**") {
        Some(("**", "*", 2))
    } else if rest.starts_with("__") {
        Some(("__", "*", 2))
    } else if rest.starts_with("~~") {
        Some(("~~", "~", 2))
    } else if rest.starts_with('*') {
        Some(("*", "_", 1))
    } else if rest.starts_with('_') {
        Some(("_", "_", 1))
    } else {
        None
    }
}

fn parse_link(rest: &str) -> Option<(String, usize)> {
    let text_end = rest[1..].find(']')? + 1;
    let after_text = text_end + 1;
    if !rest[after_text..].starts_with('(') {
        return None;
    }

    let url_start = after_text + 1;
    let url_end = rest[url_start..].rfind(')')? + url_start;
    let link_text = &rest[1..text_end];
    let link_url = &rest[url_start..url_end];

    let mut out = String::new();
    out.push('[');
    out.push_str(&escape_markdown_v2(link_text));
    out.push_str("](");
    out.push_str(&escape_url(link_url));
    out.push(')');

    Some((out, url_end + 1))
}

fn escape_markdown_v2(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for ch in text.chars() {
        push_escaped_char(&mut out, ch);
    }
    out
}

fn escape_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '`' => out.push_str("\\`"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out
}

fn push_escaped_char(out: &mut String, ch: char) {
    if needs_escape(ch) {
        out.push('\\');
    }
    out.push(ch);
}

fn needs_escape(ch: char) -> bool {
    matches!(
        ch,
        '_' | '*'
            | '['
            | ']'
            | '('
            | ')'
            | '~'
            | '`'
            | '>'
            | '#'
            | '+'
            | '-'
            | '='
            | '|'
            | '{'
            | '}'
            | '.'
            | '!'
    )
}

fn escape_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for ch in url.chars() {
        match ch {
            ')' => out.push_str("\\)"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_double_asterisk_bold() {
        assert_eq!(markdown_to_telegram("**bold text**"), "*bold text*");
    }

    #[test]
    fn converts_double_underscore_bold() {
        assert_eq!(markdown_to_telegram("__bold text__"), "*bold text*");
    }

    #[test]
    fn converts_single_asterisk_italic() {
        assert_eq!(markdown_to_telegram("*italic text*"), "_italic text_");
    }

    #[test]
    fn converts_single_underscore_italic() {
        assert_eq!(markdown_to_telegram("_italic text_"), "_italic text_");
    }

    #[test]
    fn converts_strikethrough() {
        assert_eq!(markdown_to_telegram("~~gone~~"), "~gone~");
    }

    #[test]
    fn escapes_plain_special_chars() {
        assert_eq!(
            markdown_to_telegram("Hello! How are you?"),
            "Hello\\! How are you?"
        );
    }

    #[test]
    fn preserves_formatting_and_escapes_inside() {
        assert_eq!(markdown_to_telegram("**hello!**"), "*hello\\!*");
    }

    #[test]
    fn handles_links() {
        assert_eq!(
            markdown_to_telegram("[Zo docs](https://docs.zocomputer.com/path_(x))"),
            "[Zo docs](https://docs.zocomputer.com/path_(x\\))"
        );
    }

    #[test]
    fn handles_inline_code() {
        assert_eq!(markdown_to_telegram("Use `a_b*` now"), "Use `a_b*` now");
    }

    #[test]
    fn handles_code_block() {
        let input = "```python\nprint('hello')\n```";
        assert_eq!(markdown_to_telegram(input), input);
    }

    #[test]
    fn leaves_unmatched_markers_literal_and_escaped() {
        assert_eq!(markdown_to_telegram("**not closed"), "\\*\\*not closed");
    }
}
