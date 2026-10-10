//! The agent output frame: its title, pane rendering and session lines. The
//! lifecycle status lines the event reducer appends (the started, finished
//! and human-duration formatters) live on the shell state side
//! ([`crate::app`]); the frame keeps the timer's `mm:ss` formatting.

use patok_core::config::ProviderKind;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::time::Duration;

use super::common::{fitted_prefix, render_hints_strip, split_hints_row};
use crate::app::{App, LineKind, OutLine, Pane, agent_display};
use crate::geom::wrap_chunks;
use crate::markdown::markdown_lines;
use crate::theme::Theme;
use crate::view::FrameFocus;

/// The agent output frame's key hints (T71.1): the keys its focused state
/// owns -- the scroll keys move the output, PageUp/PageDown page through it,
/// `End` resumes the live follow, and Tab moves the keys to the other frame.
/// T124.1 dropped the leading `v rail view` entry (as T75.1 did for the
/// idle `q quit` hint), and T125.1 removed the `v` binding itself.
pub(super) fn output_hints() -> Vec<&'static str> {
    vec!["↑↓ scroll", "PgUp/PgDn page", "End follow", "Tab tasks"]
}

/// The provider's display name for the output frame title ("claude" -> "Claude");
/// a slug no engine provider spells (the unimplemented names, the mock) passes
/// through unchanged.
pub(super) fn provider_display_name(provider: &str) -> &str {
    match ProviderKind::parse(provider) {
        Some(kind) => kind.display_name(),
        None => provider,
    }
}

/// The output frame title: " Builder | Claude - claude-sonnet-5.5 ", dropping the
/// model first and then the provider when `width` is too narrow; the agent type
/// always stays, so the title is never empty.
#[cfg(any(test, feature = "test-support"))]
pub fn output_title(agent: &str, provider: &str, model: &str, width: usize) -> String {
    output_title_parts(agent, provider, model, None, width).0
}

/// What survives the width squeeze (T31.1): the agent type always stays; the
/// model drops first, then the provider, then the timer.
struct KeptTitle {
    provider: Option<String>,
    model: Option<String>,
    timer: Option<String>,
}

/// The output frame title's left side from its kept parts: `" {agent}"`, then
/// `" | {provider}"` and `" - {model}"` when they survived, closed by a
/// trailing space.
#[cfg(any(test, feature = "test-support"))]
fn left_title(agent: &str, provider: Option<&str>, model: Option<&str>) -> String {
    let mut left = format!(" {agent}");
    if let Some(provider) = provider {
        left.push_str(" | ");
        left.push_str(provider);
    }
    if let Some(model) = model {
        left.push_str(" - ");
        left.push_str(model);
    }
    left.push(' ');
    left
}

/// The shared T31.1 truncation decision: which of the provider, model and
/// timer survive `width` columns. The candidates drop the model first, then
/// the provider; each is tried with the timer (`left + right <= width`),
/// then without it. The agent type always stays, so the title is never
/// empty. The fit itself is the shared [`fitted_prefix`] tail-drop over the
/// parts' costs, with the always-kept agent (its two surrounding spaces) as
/// the base.
fn kept_title(
    agent: &str,
    provider: &str,
    model: &str,
    timer: Option<&str>,
    width: usize,
) -> KeptTitle {
    let timer = timer.map(|t| format!(" {t} "));
    // The left side's fixed cost (` {agent} `), then the optional parts'
    // costs in the order they drop: the provider with its ` | `, then the
    // model with its ` - ` (which never renders without the provider).
    let base = agent.chars().count() + 2;
    let mut costs = Vec::new();
    if !provider.is_empty() {
        costs.push(3 + provider.chars().count());
        if !model.is_empty() {
            costs.push(3 + model.chars().count());
        }
    }
    let right_len = timer.as_ref().map_or(0, |t| t.chars().count());
    // With the timer: the longest prefix that fits beside it (the timer
    // survives even beside a bare agent when the pair fits).
    let with_timer = width.saturating_sub(right_len);
    if base <= with_timer {
        let (kept, _) = fitted_prefix(costs.iter().copied(), base, 0, with_timer);
        return KeptTitle {
            provider: (kept >= 1).then(|| provider.to_string()),
            model: (kept >= 2).then(|| model.to_string()),
            timer,
        };
    }
    // The timer is the last element to disappear: without it, the left parts
    // try against the full width.
    if base <= width {
        let (kept, _) = fitted_prefix(costs.iter().copied(), base, 0, width);
        return KeptTitle {
            provider: (kept >= 1).then(|| provider.to_string()),
            model: (kept >= 2).then(|| model.to_string()),
            timer: None,
        };
    }
    KeptTitle {
        provider: None,
        model: None,
        timer: None,
    }
}

/// The output frame title in two parts: the agent type with provider and model
/// on the left, the running timer on the right. A narrow `width` drops the model
/// first, then the provider, and only then the timer; the agent type always
/// keeps its surrounding spaces, so the left side is never empty.
#[cfg(any(test, feature = "test-support"))]
pub fn output_title_parts(
    agent: &str,
    provider: &str,
    model: &str,
    timer: Option<&str>,
    width: usize,
) -> (String, String) {
    let kept = kept_title(agent, provider, model, timer, width);
    (
        left_title(agent, kept.provider.as_deref(), kept.model.as_deref()),
        kept.timer.unwrap_or_default(),
    )
}

/// The output frame title in per-segment spans (T37.1): the agent type name
/// keeps its agent colour and bold while the separator, provider, model and
/// timer carry the theme's low-emphasis `detail_colour`. Concatenated, the
/// spans are exactly [`output_title_parts`]'s strings, so the T31.1 truncation
/// behaviour is shared.
pub fn output_title_spans(
    agent: &str,
    provider: &str,
    model: &str,
    timer: Option<&str>,
    width: usize,
    agent_colour: Color,
    detail_colour: Color,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let kept = kept_title(agent, provider, model, timer, width);
    let agent_style = Style::new().fg(agent_colour).bold();
    let detail_style = Style::new().fg(detail_colour);
    // The agent span closes itself when nothing else survived the squeeze.
    let mut left = vec![Span::styled(
        if kept.provider.is_none() {
            format!(" {agent} ")
        } else {
            format!(" {agent}")
        },
        agent_style,
    )];
    if let Some(provider) = kept.provider.as_deref() {
        if let Some(model) = kept.model.as_deref() {
            left.push(Span::styled(format!(" | {provider}"), detail_style));
            left.push(Span::styled(format!(" - {model} "), detail_style));
        } else {
            left.push(Span::styled(format!(" | {provider} "), detail_style));
        }
    }
    let right = kept
        .timer
        .map(|timer| vec![Span::styled(timer, detail_style)])
        .unwrap_or_default();
    (left, right)
}

/// A session's elapsed time as `mm:ss` with zero-padded minutes.
pub fn format_elapsed(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

pub(super) fn render_output(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let timer = app.session_elapsed().map(format_elapsed);
    // The agent type name keeps its fixed agent colour in every theme (T11.1,
    // T24.1, T110.1); the frame title is the one surface that keeps the
    // fixated name colour (T117.1), and the separator, provider, model and
    // timer render in the theme's low-emphasis detail colour (T37.1).
    let title_colour = Theme::agent_name_color(theme, &app.run.agent);
    let (left, right) = output_title_spans(
        &agent_display(&app.run.agent),
        provider_display_name(&app.run.provider),
        &app.run.model,
        timer.as_deref(),
        usize::from(area.width.saturating_sub(2)),
        title_colour,
        theme.muted_text,
    );
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title_top(Line::from(left));
    // The running session timer sits right-aligned on the same title line.
    if !right.is_empty() {
        block = block.title_top(Line::from(right).right_aligned());
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // The focused frame reserves its bottom inner row for the key hints strip
    // (T71.1); the unfocused frame keeps the full inner area and no strip.
    let (content, hints_row) = split_hints_row(inner, app.focus == FrameFocus::Output);
    let max_scroll = render_pane(
        frame,
        theme,
        &app.run.output,
        app.scroll,
        "no output yet",
        content,
    );
    app.layout.max_scroll.set(max_scroll);
    render_hints_strip(frame, hints_row, &output_hints(), theme);
}

/// Draws agent output scrolled up `scroll` visual lines from the bottom; returns the largest useful scroll.
pub(super) fn render_pane(
    frame: &mut Frame,
    theme: Theme,
    pane: &Pane,
    scroll: usize,
    empty: &str,
    inner: Rect,
) -> usize {
    if pane.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                empty.to_string(),
                Style::new().fg(theme.muted_text),
            )),
            inner,
        );
        return 0;
    }

    let width = usize::from(inner.width.max(1));
    let height = usize::from(inner.height);
    let visual: Vec<Line> = pane
        .lines
        .iter()
        .flat_map(|line| visual_lines(theme, line, width))
        .collect();
    let max_scroll = visual.len().saturating_sub(height);
    let scroll = scroll.min(max_scroll);
    let end = visual.len() - scroll;
    let start = end.saturating_sub(height);
    frame.render_widget(Paragraph::new(visual[start..end].to_vec()), inner);
    max_scroll
}

/// One logical output line as visual rows: plain text hard-wraps; thinking renders
/// as markdown first, so headings, emphasis, lists and code blocks show up styled.
/// Every line, the agent's name inside it included, wears its line kind's colour;
/// the fixated per-agent name colour reaches only the output frame title (T117.1).
pub(super) fn visual_lines(theme: Theme, line: &OutLine, width: usize) -> Vec<Line<'static>> {
    let style = style_of(theme, line.kind);
    if line.kind == LineKind::Thinking {
        markdown_lines(&line.text, style)
            .into_iter()
            .flat_map(|row| wrap_line(&row, width))
            .collect()
    } else {
        wrap(&line.text, width)
            .into_iter()
            .map(|piece| Line::styled(piece, style))
            .collect()
    }
}

/// Wraps a styled line at `width` columns, keeping each span's style.
pub(super) fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    if cells.is_empty() {
        return vec![Line::default()];
    }
    wrap_chunks(cells.len(), width.max(1))
        .map(|range| {
            let mut spans: Vec<Span> = Vec::new();
            for (c, style) in &cells[range] {
                match spans.last_mut() {
                    Some(last) if last.style == *style => last.content.to_mut().push(*c),
                    _ => spans.push(Span::styled(c.to_string(), *style)),
                }
            }
            Line::from(spans)
        })
        .collect()
}

pub(super) fn style_of(theme: Theme, kind: LineKind) -> Style {
    // `Text` renders unstyled: no fg at all, so the line inherits the base
    // style's foreground instead of resetting it.
    if kind == LineKind::Text {
        return Style::new();
    }
    let style = Style::new().fg(Theme::line_color(theme, kind));
    if kind == LineKind::Heading {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

/// Hard-wraps at `width` characters; an empty line stays one empty line.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    wrap_chunks(chars.len(), width)
        .map(|range| chars[range].iter().collect())
        .collect()
}
