//! The task list frame.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use super::common::{collapsed_title_width, render_collapsed, render_hints_strip, split_hints_row};
use crate::app::App;
use crate::view::FrameFocus;

/// The task list frame's key hints (T71.1): the keys its focused state owns,
/// the most important leading so the trailing hints drop first on a narrow
/// frame. Every label matches what `App::on_key` does with the key while no
/// modal holds the keyboard and the task list frame has the focus: `a` opens
/// the add-task dialog (T74.1) and `i` the inject-task modal (T76.1), both
/// from the focused task list frame, Enter starts the build loop or runs a
/// discovery round while the engine is idle, the scroll keys move the task
/// list, and Tab moves the keys to the other frame.
pub(super) fn task_hints(app: &App) -> Vec<&'static str> {
    let mut hints = vec!["a add tasks", "i inject task"];
    if app.is_idle() {
        hints.push(if app.run.tasks.iter().any(|t| !t.done) {
            "Enter start"
        } else {
            "Enter run discovery"
        });
    }
    hints.push("↑↓ scroll");
    hints.push("Tab output");
    hints
}

/// The task list frame's title spans: ` Tasks | done/total - N left `, with
/// only the word `Tasks` in the frame's title colour; everything after it
/// -- the pipe separator, the three counts, the slash, the dash and the word
/// `left` -- wears the theme's low-emphasis detail colour (T67.1). A collapsed
/// frame passes the current task's id (` | T1.2 `) to append.
fn tasks_title(app: &App, current: Option<&str>) -> Vec<Span<'static>> {
    let detail = Style::new().fg(app.theme().muted_text);
    let done = app.run.tasks.iter().filter(|t| t.done).count();
    let total = app.run.tasks.len();
    let left = app.run.tasks.iter().filter(|t| !t.done).count();
    let mut spans = vec![
        Span::raw(" Tasks"),
        Span::styled(" | ", detail),
        Span::styled(done.to_string(), detail),
        Span::styled("/", detail),
        Span::styled(total.to_string(), detail),
        Span::styled(" - ", detail),
        Span::styled(left.to_string(), detail),
    ];
    match current {
        Some(id) => {
            spans.push(Span::styled(" left", detail));
            spans.push(Span::styled(format!(" | {id} "), detail));
        }
        None => spans.push(Span::styled(" left ", detail)),
    }
    spans
}

/// Renders the task list frame, or only its single line when `collapsed`.
pub(super) fn render_tasks(frame: &mut Frame, app: &App, area: Rect, collapsed: bool) {
    let theme = app.theme();
    if collapsed {
        // The title grows by the current task's id while it fits; a single
        // line has no list to scroll.
        let width = collapsed_title_width(area.width);
        let mut title = tasks_title(app, app.run.current_task.as_deref());
        if title
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
            > width
        {
            title = tasks_title(app, None);
        }
        app.layout.task_max_scroll.set(0);
        render_collapsed(frame, area, title, Vec::new(), "↑", theme);
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(tasks_title(app, None)));
    let inner = block.inner(area);
    // The focused frame reserves its bottom inner row for the key hints strip
    // (T71.1); the unfocused frame keeps the full inner area and no strip.
    let (content, hints_row) = split_hints_row(inner, app.focus == FrameFocus::Tasks);

    // Keep the first pending task in view, a third of the way down; `task_scroll`
    // rows scrolled up from that auto view (toward earlier tasks).
    let focus = app
        .run
        .tasks
        .iter()
        .position(|t| !t.done)
        .unwrap_or(app.run.tasks.len());
    let auto = focus.saturating_sub(usize::from(content.height) / 3);
    app.layout.task_max_scroll.set(auto);
    let offset = auto.saturating_sub(app.task_scroll.min(auto));
    let items: Vec<ListItem> = app
        .run
        .tasks
        .iter()
        .skip(offset)
        .map(|task| {
            let running = app.run.current_task.as_deref() == Some(task.id.as_str());
            let (mark, style) = match (task.done, running) {
                (true, _) => ("✔", Style::new().fg(theme.muted_text)),
                (false, true) => ("▶", Style::new().fg(theme.success).bold()),
                (false, false) => ("○", Style::new()),
            };
            let style = if app.is_new_task(&task.id) {
                Style::new()
                    .fg(theme.highlighted_text)
                    .bold()
                    .add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            ListItem::new(Line::styled(
                format!("{mark} {}  {}", task.id, task.description),
                style,
            ))
        })
        .collect();
    frame.render_widget(block, area);
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "no tasks in TASKS.md",
                Style::new().fg(theme.muted_text),
            )),
            content,
        );
    } else {
        frame.render_widget(List::new(items), content);
    }
    render_hints_strip(frame, hints_row, &task_hints(app), theme);
}
