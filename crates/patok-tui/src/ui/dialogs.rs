//! The add-task / inject-task dialog, the stop dialog and the theme picker.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::common::{
    ENTRY_INDENT, base_style, group_header_line, render_modal_footer, selected_row_style,
};
use super::modal::{ModalTitle, render_modal_frame};
use crate::app::{App, DialogKind, menu_entries};
use crate::interactive::{ScreenView, cursor_position};
use crate::modal::{Modal, ModalLayer};
use crate::overlay::Entry;
use crate::theme::theme_modal_groups;
use crate::view::STOP_CHOICES;
use patok_core::config::{EnumSpelling, ProviderKind};
use patok_core::event::Phase;

/// The placeholder hint rendered in the empty input area of the add-task dialog
/// (T41.1): a low-emphasis watermark that reads as a hint, not typed content.
/// It disappears with the first typed or pasted character and never enters
/// the input's text or a submitted request.
pub const DIALOG_WATERMARK: &str = "Press Enter to run Discovery agent";

/// The inject-task modal's empty-input watermark (T76.1), in the place of the
/// add-task dialog's: it says what the confirm does with the typed text, so
/// the normalized append is never a surprise.
pub const INJECT_WATERMARK: &str = "Type the task; it lands as an unchecked task line";

/// The add-task dialog rect: about 90% of the width and 80% of the height, at least
/// 60x20, clamped to the screen and centered.
pub fn dialog_area(screen: Rect) -> Rect {
    let width = (screen.width * 9 / 10).max(60).min(screen.width);
    let height = (screen.height * 4 / 5).max(20).min(screen.height);
    crate::geom::centered(screen, width, height)
}

pub(crate) fn render_dialog(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = Modal::Dialog.area(app, frame.area());
    // The two modals share everything but their chrome strings (T76.1): the
    // title and the empty-input watermark follow the dialog's kind; the
    // primary button's label flows from `App::primary_label`, which branches
    // on the same kind.
    let (title, watermark) = match app.dialog.kind {
        DialogKind::Add => (" What do you want to do? ", DIALOG_WATERMARK),
        DialogKind::Inject => (" Inject a task ", INJECT_WATERMARK),
    };
    let chrome = render_modal_frame(frame, theme, area, ModalTitle::Left(title));
    app.dialog.close.set(chrome.close);
    let inner = chrome.inner;
    let [input, dialog_status, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    if let Some(message) = &app.dialog.status {
        frame.render_widget(
            Paragraph::new(Line::styled(
                message.clone(),
                Style::new().fg(theme.highlighted_text),
            )),
            dialog_status,
        );
    }

    let width = usize::from(input.width.max(2)) - 1;
    app.dialog.editor.set_width(width);
    let chars: Vec<char> = app.dialog.editor.text().chars().collect();
    let rows = app.dialog.editor.rows();
    let cursor = app.dialog.editor.cursor();
    let cursor_row = app.dialog.editor.cursor_row(&rows);
    // The block cursor covers the cell at the cursor: the character under it
    // on the cursor style, or a styled space at the end of a logical line or
    // on an empty one. No cell is added, so the letters around the cursor
    // stay in place while it moves. The cursor cell wears the highlighted-text
    // foreground (T138.1), the colour selected rows and the scrollbar thumb
    // already share.
    // REVERSED keeps the cell a visible block: a bare foreground on a space
    // would draw nothing.
    let cursor_style = Style::new()
        .fg(theme.highlighted_text)
        .add_modifier(Modifier::REVERSED);
    let mut lines: Vec<Line> = if app.dialog.editor.text().is_empty() {
        // The empty-input watermark (T41.1): a dim hint followed by the block
        // cursor, which keeps the focus indicator visible.
        vec![Line::from(vec![
            Span::styled(watermark, Style::new().fg(theme.muted_text)),
            Span::styled(" ", cursor_style),
        ])]
    } else {
        rows.iter()
            .enumerate()
            .map(|(i, row)| {
                let text = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
                if i == cursor_row {
                    let cell = if cursor < row.end {
                        chars[cursor].to_string()
                    } else {
                        " ".to_string()
                    };
                    Line::from(vec![
                        Span::raw(text(row.start, cursor)),
                        Span::styled(cell, cursor_style),
                        Span::raw(text((cursor + 1).min(row.end), row.end)),
                    ])
                } else {
                    Line::from(text(row.start, row.end))
                }
            })
            .collect()
    };
    // Keep the cursor's row in view.
    let skip = (cursor_row + 1).saturating_sub(usize::from(input.height));
    let visible: Vec<Line> = lines.drain(skip..).collect();
    frame.render_widget(Paragraph::new(visible), input);

    // The bottom line's two zones (T59.1): the input's key hints on the left,
    // the primary-action and Close buttons on the right, all through the
    // shared footer renderer. The primary button's label follows the scenario
    //.
    app.dialog.footer.set(footer);
    let buttons = Modal::Dialog.buttons(app);
    render_modal_footer(
        frame,
        footer,
        &["↑↓←→ move", "Shift-Enter new line", "Ctrl+U clear"],
        &buttons,
        theme,
    );
}

/// The stop dialog's rect: a small centered modal, at least 54 columns wide and
/// 9 rows tall, clamped to the screen -- the shared choice-dialog rect
/// (finding 6).
#[cfg(any(test, feature = "test-support"))]
pub fn stop_area(screen: Rect) -> Rect {
    crate::choice::choice_area(screen)
}

/// The stop dialog (Esc while a build runs, T46.1): a centered " Stop build "
/// modal with the three choices as a vertical list -- the selected choice in
/// the shared selected-row style (T118.1) -- and a two-zone bottom line of
/// hints left, buttons right (T59.1). Rendered on top of everything else.
/// The shared choice-dialog renderer draws it (finding 6); this records the
/// rects its mouse hit-testing reads.
pub(crate) fn render_stop_dialog(frame: &mut Frame, app: &App) {
    let rects = super::choice::render_choice_dialog(
        frame,
        app.theme(),
        " Stop build ",
        &STOP_CHOICES,
        app.stop.selected,
        Modal::Stop.area(app, frame.area()),
        &Modal::Stop.buttons(app),
    );
    app.stop.close.set(rects.close);
    app.stop.body.set(rects.body);
    app.stop.footer.set(rects.footer);
}

/// The m menu's rect: a small centered modal, at least 54 columns wide and
/// tall enough for its rows -- the border (2), the entry rows and the footer
/// (1) -- clamped to the screen; the stop entry adds one row while a
/// build runs.
pub fn menu_area(screen: Rect, running: bool) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let height = (menu_entries(running).len() + 3).min(usize::from(screen.height)) as u16;
    crate::geom::centered(screen, width, height)
}

/// The m menu (the `m` key): a centered " Menu " modal listing the status
/// bar's former secondary hints as rows -- the selected choice in the shared
/// selected-row style (T118.1) -- and a two-zone bottom line of hints left,
/// buttons right (T59.1), styled like the stop dialog. Each entry runs the
/// action its direct key binding triggers; the stop row renders only
/// while a build runs. Each rendered row is clickable: a left click moves
/// the selection to the row and runs it (T137.1), so the body rect is
/// recorded for the app's mouse hit-testing. Rendered on top of everything
/// else.
pub(crate) fn render_menu(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let running = app.run.phase == Phase::Running;
    let area = Modal::Menu.area(app, frame.area());
    let chrome = render_modal_frame(frame, theme, area, ModalTitle::Centered(" Menu "));
    app.menu.close.set(chrome.close);
    let inner = chrome.inner;
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    app.menu.body.set(body);
    let lines: Vec<Line> = menu_entries(running)
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let focused = index == app.menu.selected;
            let marker = if focused { "▶ " } else { "  " };
            let mut line = Line::from(vec![
                Span::raw(marker),
                Span::raw(choice.label()),
                Span::styled(
                    format!(" -- {}", choice.detail()),
                    Style::new().fg(if focused {
                        theme.highlighted_text
                    } else {
                        theme.muted_text
                    }),
                ),
            ]);
            if focused {
                line = line.style(selected_row_style(theme));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
    // The bottom line's two zones (T59.1): the selection hint on the left, the
    // Confirm and Close buttons on the right.
    app.menu.footer.set(footer);
    render_modal_footer(
        frame,
        footer,
        &["↑↓ move"],
        &Modal::Menu.buttons(app),
        theme,
    );
}

/// The interactive picker's rect: as wide as the menu, and tall enough for
/// one row per provider -- the border (2), the rows and the footer (1) --
/// plus a blank row and the warning line while a build runs, clamped to the
/// screen.
pub fn interactive_picker_area(screen: Rect, running: bool) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let rows = ProviderKind::VARIANTS.len() + 3 + if running { 2 } else { 0 };
    let height = rows.min(usize::from(screen.height)) as u16;
    crate::geom::centered(screen, width, height)
}

/// The interactive picker (the `I` key): a centered " Interactive " modal
/// listing each provider of the capability table with its program, or a
/// muted `not found on PATH` note when it is unavailable, the focused row in
/// the shared selected-row style. While a build runs a warning line follows
/// the rows. The close, body and footer rects are recorded for the app's
/// mouse hit-testing. Rendered on top of everything else.
pub(crate) fn render_interactive_picker(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let running = app.run.phase == Phase::Running;
    let area = Modal::InteractivePicker.area(app, frame.area());
    let chrome = render_modal_frame(frame, theme, area, ModalTitle::Centered(" Interactive "));
    app.interactive.close.set(chrome.close);
    let inner = chrome.inner;
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    app.interactive.body.set(body);
    let mut lines: Vec<Line> = app
        .interactive
        .providers
        .iter()
        .enumerate()
        .map(|(index, &(kind, available))| {
            let focused = index == app.interactive.selected;
            let marker = if focused { "▶ " } else { "  " };
            let program = kind.interactive_launch().program;
            let (label_style, detail) = if available {
                (Style::new(), program.to_string())
            } else {
                (
                    Style::new().fg(theme.muted_text),
                    format!("`{program}` not found on PATH"),
                )
            };
            let mut line = Line::from(vec![
                Span::raw(marker),
                Span::styled(kind.display_name(), label_style),
                Span::styled(
                    format!(" -- {detail}"),
                    Style::new().fg(if focused && available {
                        theme.highlighted_text
                    } else {
                        theme.muted_text
                    }),
                ),
            ]);
            if focused {
                line = line.style(selected_row_style(theme));
            }
            line
        })
        .collect();
    if running {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "⚠ A build is running; the session shares its files.",
            Style::new().fg(theme.highlighted_text),
        ));
    }
    frame.render_widget(Paragraph::new(lines), body);
    app.interactive.footer.set(footer);
    render_modal_footer(
        frame,
        footer,
        &["↑↓ move"],
        &Modal::InteractivePicker.buttons(app),
        theme,
    );
}

/// The theme picker's rect (T43.1, T116.1): a centered modal, at least 54
/// columns wide and tall enough for its full grouped list -- the border (2),
/// the two group headers, the eleven one-line entries (13 body rows) and the
/// footer (1), so 16 -- clamped to the screen.
pub fn theme_area(screen: Rect) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let height = 16.min(screen.height);
    crate::geom::centered(screen, width, height)
}

/// The theme picker (the `t` key, T43.1, T116.1): a centered " Theme " modal
/// listing the built-in themes in two collapsible groups, Dark then Light,
/// mirroring the settings overlay's grouped list -- each header carries the
/// fold glyph and toggles its group, the entries nest indented under it.
/// Every row renders in the currently active theme's classes (T115.1), with
/// a selection marker and a two-zone bottom line of hints left, buttons
/// right (T59.1). Rendered on top of everything else; the shell underneath
/// is already recoloured by the live preview, since `App::theme` resolves it.
pub(crate) fn render_theme_modal(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = Modal::Theme.area(app, frame.area());
    let chrome = render_modal_frame(frame, theme, area, ModalTitle::Centered(" Theme "));
    app.theme_modal.close.set(chrome.close);
    let inner = chrome.inner;
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    app.theme_modal.area.set(body);
    let groups = theme_modal_groups();
    let entries = app.theme_modal.entries();
    let width = groups
        .iter()
        .flat_map(|(_, names)| names.iter())
        .map(|name| name.len())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| theme_row_line(app, index, *entry, &groups, width))
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
    // The bottom line's two zones (T59.1): the preview hints on the left, the
    // Save and Cancel buttons on the right. The modal is the narrowest of the
    // five, so the hint stays short enough to keep the two zones apart.
    app.theme_modal.footer.set(footer);
    render_modal_footer(
        frame,
        footer,
        &["↑↓/jk previews"],
        &Modal::Theme.buttons(app),
        theme,
    );
}

/// One visible entry of the picker (T43.1, T115.1, T116.1): a group header
/// line -- the marker, the fold glyph and the title, bold -- or a theme
/// entry, the name indented under its header and padded to the list's
/// longest name, rendered as a normal list row in the currently active
/// theme's classes instead of previewing the entry's own palette. The
/// selected entry carries the marker and renders in `highlighted_text`,
/// bold, through the shared selected-row style (T118.1).
pub(super) fn theme_row_line(
    app: &App,
    index: usize,
    entry: Entry<&'static str>,
    groups: &[(&'static str, Vec<&'static str>)],
    width: usize,
) -> Line<'static> {
    let theme = app.theme();
    let selected = index == app.theme_modal.selected;
    let marker = if selected { "▶ " } else { "  " };
    match entry {
        Entry::Header(group) => {
            let (title, expanded) = groups
                .get(group)
                .map(|(title, _)| {
                    (
                        *title,
                        app.theme_modal
                            .expanded
                            .get(group)
                            .copied()
                            .unwrap_or(false),
                    )
                })
                .unwrap_or(("", false));
            let style = if selected {
                selected_row_style(theme)
            } else {
                base_style(theme)
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD)
            };
            group_header_line(marker, expanded, title).style(style)
        }
        Entry::Row(name) => {
            let fg = if selected {
                theme.highlighted_text
            } else {
                theme.normal_text
            };
            let row_marker = format!("{marker}{}", " ".repeat(ENTRY_INDENT));
            let spans = vec![
                Span::styled(row_marker, Style::new().fg(fg)),
                Span::styled(format!("{name:<width$}"), Style::new().fg(fg)),
            ];
            let mut line = Line::from(spans).style(base_style(theme));
            if selected {
                line = line.style(selected_row_style(theme));
            }
            line
        }
    }
}

/// The embedded terminal's rect: the whole screen less a one-cell margin,
/// or the whole screen when it is too small for one.
pub fn terminal_area(screen: Rect) -> Rect {
    if screen.width >= 24 && screen.height >= 8 {
        screen.inner(Margin::new(1, 1))
    } else {
        screen
    }
}

/// The PTY's rect inside the terminal modal at `area`: the border's inner
/// less the footer row. The renderer and the PTY sizing both use it.
pub(crate) fn terminal_body(area: Rect) -> Rect {
    let [body, _footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
        .areas(area.inner(Margin::new(1, 1)));
    body
}

fn terminal_title(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => " Claude ",
        ProviderKind::Codex => " Codex ",
        ProviderKind::Mistral => " Mistral ",
    }
}

/// The embedded terminal: a large modal whose body is the child's screen,
/// with the real cursor placed while the child runs. The close, body and
/// footer rects are recorded for the mouse handling.
pub(crate) fn render_terminal(frame: &mut Frame, app: &App) {
    let Some(terminal) = app.terminal.as_ref() else {
        return;
    };
    let theme = app.theme();
    let area = Modal::Terminal.area(app, frame.area());
    let chrome = render_modal_frame(
        frame,
        theme,
        area,
        ModalTitle::Centered(terminal_title(terminal.kind)),
    );
    terminal.close.set(chrome.close);
    let body = terminal_body(area);
    let [_, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(chrome.inner);
    terminal.body.set(body);
    terminal.footer.set(footer);
    terminal.session.with_screen(|screen| {
        frame.render_widget(ScreenView::new(screen).cursor(false), body);
        if !terminal.exited
            && let Some(position) = cursor_position(screen, body)
        {
            frame.set_cursor_position(position);
        }
    });
    // After the child exits the final screen stays up until the user closes
    // it; the hint names the chord.
    let exited = terminal
        .exit_code
        .map(|code| format!("[process exited (code {code})] — Ctrl+] to close"));
    let hints: Vec<&str> = exited.iter().map(String::as_str).collect();
    render_modal_footer(frame, footer, &hints, &Modal::Terminal.buttons(app), theme);
}
