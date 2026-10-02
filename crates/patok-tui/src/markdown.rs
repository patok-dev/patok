//! A small markdown renderer for agent output: headings, emphasis, lists, code spans
//! and fenced code blocks become styled terminal lines. Every span is derived from one
//! base style, so the caller's colour scheme (the thinking colour) stays intact.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Renders `markdown` into styled lines on top of `base`. Headings and strong text are
/// bold, emphasised text italic, list items bulleted, code-span backticks and code-fence
/// markers stripped (fenced blocks indent by two spaces). Empty input is one empty line,
/// like the plain-text wrap.
pub fn markdown_lines(markdown: &str, base: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    // While set, every line belongs to a fenced code block; the value is its fence.
    let mut fence: Option<String> = None;
    for raw in markdown.split('\n') {
        let line = raw.trim_end();
        if let Some(marker) = &fence {
            if line.trim_start().starts_with(marker.as_str()) {
                fence = None;
            } else {
                lines.push(code_line(line, base));
            }
            continue;
        }
        if let Some(marker) = fence_marker(line) {
            fence = Some(marker);
            continue;
        }
        if let Some(text) = heading_text(line) {
            lines.push(Line::from(inline_spans(
                text,
                base.add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some((marker, text)) = list_item(line) {
            let mut spans = vec![Span::styled(marker, base)];
            spans.extend(inline_spans(text, base));
            lines.push(Line::from(spans));
            continue;
        }
        if line.is_empty() {
            lines.push(Line::styled(String::new(), base));
            continue;
        }
        lines.push(Line::from(inline_spans(line, base)));
    }
    if lines.is_empty() {
        lines.push(Line::styled(String::new(), base));
    }
    lines
}

/// The fence marker a line opens a code block with (three backticks or tildes); the
/// rest of the line is a language tag, never content.
fn fence_marker(line: &str) -> Option<String> {
    ["```", "~~~"]
        .iter()
        .find(|marker| line.trim_start().starts_with(*marker))
        .map(|marker| marker.to_string())
}

/// Heading text when the line is an ATX heading (`#` to `######` followed by a space).
fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    rest.starts_with(' ').then(|| rest.trim())
}

/// A list item's bullet and text, unordered (`-`, `*`, `+`) or ordered (`1.`, `2)`).
fn list_item(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    for (marker, sep) in [('-', ' '), ('*', ' '), ('+', ' ')] {
        if let Some(rest) = trimmed
            .strip_prefix(marker)
            .and_then(|rest| rest.strip_prefix(sep))
        {
            // A line starting with `*text` is emphasis, not a list.
            if !rest.is_empty() {
                return Some((format!("{marker} "), rest));
            }
        }
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let (bullet, rest) = &trimmed[digits..]
            .strip_prefix('.')
            .map(|rest| (". ", rest))
            .or_else(|| trimmed[digits..].strip_prefix(')').map(|rest| (") ", rest)))?;
        if let Some(rest) = rest.strip_prefix(' ').filter(|rest| !rest.is_empty()) {
            return Some((format!("{}{}", &trimmed[..digits], bullet), rest));
        }
    }
    None
}

/// One line of a fenced code block: two spaces of indent, no markdown parsing.
fn code_line(line: &str, base: Style) -> Line<'static> {
    Line::styled(format!("  {line}"), base)
}

/// Inline markdown to styled spans: `code`, *emphasis*, **strong**; anything unparsed
/// stays literal.
fn inline_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    let flush = |plain: &mut String, spans: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(plain), base));
        }
    };
    while i < chars.len() {
        match chars[i] {
            '`' if code_end(&chars, i).is_some_and(|end| end > i + 1) => {
                let end = code_end(&chars, i).unwrap();
                flush(&mut plain, &mut spans);
                spans.push(Span::styled(
                    chars[i + 1..end].iter().collect::<String>(),
                    base,
                ));
                i = end + 1;
            }
            '`' if code_end(&chars, i).is_some() => {
                // An empty code span renders as nothing.
                i += 2;
            }
            c @ ('*' | '_') => {
                let doubled = chars.get(i + 1) == Some(&c);
                if let Some((end, style)) = emphasis_end(&chars, i, c, doubled, base) {
                    flush(&mut plain, &mut spans);
                    spans.push(Span::styled(
                        chars[i + 1 + usize::from(doubled)..end]
                            .iter()
                            .collect::<String>(),
                        style,
                    ));
                    i = end + 1 + usize::from(doubled);
                } else {
                    plain.push(c);
                    i += 1;
                }
            }
            c => {
                plain.push(c);
                i += 1;
            }
        }
    }
    flush(&mut plain, &mut spans);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

/// The index of the backtick closing the code span opened at `open`.
fn code_end(chars: &[char], open: usize) -> Option<usize> {
    (open + 1..chars.len()).find(|&j| chars[j] == '`')
}

/// The closing index and style of the emphasis opened at `open`, when it is real
/// emphasis rather than a stray marker (so `snake_case_words` stay literal).
fn emphasis_end(
    chars: &[char],
    open: usize,
    marker: char,
    doubled: bool,
    base: Style,
) -> Option<(usize, Style)> {
    let content_start = open + 1 + usize::from(doubled);
    // Nothing to emphasise, or the marker opens with a space.
    if chars.get(content_start).is_none_or(|c| c.is_whitespace()) {
        return None;
    }
    // `_` only emphasises at a word boundary; `*` anywhere.
    if marker == '_' && open > 0 && chars[open - 1].is_alphanumeric() {
        return None;
    }
    let mut j = content_start;
    while j < chars.len() {
        if chars[j] == marker {
            let closes = if doubled {
                chars.get(j + 1) == Some(&marker) && !chars[j - 1].is_whitespace()
            } else {
                chars.get(j + 1) != Some(&marker)
                    && !chars[j - 1].is_whitespace()
                    // `_` closes only at a word boundary.
                    && (marker == '*'
                        || chars.get(j + 1).is_none_or(|c| !c.is_alphanumeric()))
            };
            if closes {
                let style = if doubled {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base.add_modifier(Modifier::ITALIC)
                };
                return Some((j, style));
            }
            j += usize::from(doubled);
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn headings_lists_and_paragraphs() {
        let lines = markdown_lines(
            "# Title\nplain\n- one\n* two\n+ three\n1. step",
            Style::new(),
        );
        assert_eq!(
            text(&lines),
            ["Title", "plain", "- one", "* two", "+ three", "1. step"]
        );
        // Heading and list text is styled; paragraphs keep the base style.
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            !lines[1].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn emphasis_and_code_spans() {
        let line = &markdown_lines("a *em* b **strong** c `code` _it_ d", Style::new())[0];
        assert_eq!(
            line.spans
                .iter()
                .map(|s| (s.content.as_ref(), s.style.add_modifier))
                .collect::<Vec<_>>(),
            [
                ("a ", Modifier::empty()),
                ("em", Modifier::ITALIC),
                (" b ", Modifier::empty()),
                ("strong", Modifier::BOLD),
                (" c ", Modifier::empty()),
                ("code", Modifier::empty()),
                (" ", Modifier::empty()),
                ("it", Modifier::ITALIC),
                (" d", Modifier::empty()),
            ]
        );
    }

    #[test]
    fn stray_markers_stay_literal() {
        let lines = markdown_lines("snake_case_name and 2 * 3 * 4", Style::new());
        assert_eq!(text(&lines), ["snake_case_name and 2 * 3 * 4"]);
        // Six hashes is a heading, seven is not; an unterminated fence is code to the end.
        assert_eq!(
            text(&markdown_lines("####### deep", Style::new())),
            ["####### deep"]
        );
        assert_eq!(
            text(&markdown_lines(
                "```rust\nlet x = 1;\nstill code",
                Style::new()
            )),
            ["  let x = 1;", "  still code"]
        );
    }

    #[test]
    fn fenced_code_blocks_strip_their_markers() {
        let lines = markdown_lines(
            "before\n```rust\nlet x = `1`;\n**kept**\n```\nafter",
            Style::new(),
        );
        assert_eq!(
            text(&lines),
            ["before", "  let x = `1`;", "  **kept**", "after"]
        );
    }

    #[test]
    fn empty_input_is_one_empty_line() {
        let lines = markdown_lines("", Style::new());
        assert_eq!(text(&lines), [""]);
    }
}
