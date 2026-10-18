//! The merged bottom status line.

use patok_core::event::Phase;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::common::fitted_prefix;
use crate::app::App;
use crate::hit::{button_line, button_text};

/// The merged status line (T86.1): one bottom row holding the status content,
/// right-aligned: a transient message, else the key-hint buttons. The app
/// status and the run mode live on the pipeline rail above the frames. Hint buttons drop as whole buttons from
/// the tail when they do not fit; a message is never dropped -- it truncates
/// at the line's right edge like a bare status bar message. The secondary
/// hints (settings, theme, detach, quit) live behind the m menu, so the zone
/// carries the idle Enter button, the running Esc stop button (T134.1) and
/// the m menu button. All three are interactive (T133.1, T134.1): each renders
/// through the shared [`button_line`] so it reads as a modal button, and each
/// records a rect covering its full [`button_text`] width on
/// [`App::layout`'s `menu_chip`, `enter_chip` or `stop_chip` for the mouse
/// hit-test -- a left click anywhere on the button runs its key's action.
pub(super) fn status_widget(app: &App, area: Rect) -> Paragraph<'static> {
    let width = usize::from(area.width);
    let theme = app.theme();
    // The interactive chips' rects reset every render: a showing message or
    // a pair dropped for width leaves the zero rect, which contains no real
    // position, so a click misses naturally.
    app.layout.menu_chip.set(Rect::default());
    app.layout.enter_chip.set(Rect::default());
    app.layout.stop_chip.set(Rect::default());
    let right = match &app.status {
        Some(message) => {
            // Bare text: one separating space, right-aligned padding when it fits -- none when it would
            // underflow, so the Paragraph truncates at the right edge.
            let pad = width.saturating_sub(1 + message.chars().count());
            let mut spans = vec![Span::raw(" ".repeat(pad + 1))];
            // The message renders as one piece in the highlighted-text colour,
            // the agent's display name inside it included (T117.1); the
            // fixated per-agent name colour reaches only the frame title.
            spans.push(Span::styled(
                message.clone(),
                Style::new().fg(theme.highlighted_text),
            ));
            spans
        }
        None => {
            let mut keys = Vec::new();
            // The primary-action hint leads while the engine is idle: Enter
            // starts the build loop while pending tasks remain and runs a
            // discovery round once the queue is complete (T46.1).
            if app.is_idle() {
                keys.push((
                    "Enter",
                    if app.run.tasks.iter().any(|t| !t.done) {
                        "start"
                    } else {
                        "run discovery"
                    },
                ));
            }
            // The Esc stop chip (T134.1): the Esc binding's scope while a run
            // is active -- a build, a planner run or a discovery run -- a
            // pending soft stop's Esc cancels the stop instead, so the chip
            // hides while one is pending. A click on it opens the stop
            // dialog, exactly like the key; the m menu's stop entry keeps
            // its build-only scope.
            if (app.run.phase == Phase::Running || app.run.planning || app.run.discovering)
                && !app.run.stopping
            {
                keys.push(("Esc", "stop"));
            }
            // The m menu button: the settings, theme, detach and quit hints
            // live behind the menu modal, so the bar keeps one button for
            // all of them.
            keys.push(("m", "menu"));
            // A button drops as a whole from the tail when it does not fit
            // the zone left of the chips, so no half-cut button ever shows;
            // the kept prefix is right-aligned. The buttons' own padding
            // separates them from the mode chip, so no extra gap column is
            // added. The drop itself is the shared [`fitted_prefix`]
            // tail-drop with no base and no gap.
            let zone = width;
            let costs = keys
                .iter()
                .map(|(key, label)| button_text(key, label).chars().count());
            let (kept, used) = fitted_prefix(costs, 0, 0, zone);
            let kept = &keys[..kept];
            let mut right = vec![Span::raw(" ".repeat(zone.saturating_sub(used)))];
            // The hint zone's running column: the pad ends where the first
            // kept button starts, and each button advances it by its own
            // character count, so every interactive chip's rect is the exact
            // columns its full button text occupies.
            let mut column = zone.saturating_sub(used);
            for &(key, label) in kept {
                // The interactive chips (T133.1, T134.1, T141.1) -- the m menu
                // button, the idle Enter button and the running Esc stop
                // button -- render through the shared [`button_line`] so each
                // reads as the modal button it is, and record a rect covering
                // the full [`button_text`] width -- key, brackets and label
                // together, the same width [`footer_button_rects`] uses --
                // so a left click anywhere on the button runs its key's
                // action.
                let width = u16::try_from(button_text(key, label).chars().count()).unwrap_or(0);
                let rect = Rect::new(
                    area.x + u16::try_from(column).unwrap_or(0),
                    area.y,
                    width,
                    1,
                );
                match key {
                    "m" => app.layout.menu_chip.set(rect),
                    "Enter" => app.layout.enter_chip.set(rect),
                    _ => app.layout.stop_chip.set(rect),
                }
                right.extend(button_line(key, label, theme).spans);
                column += usize::from(width);
            }
            right
        }
    };
    Paragraph::new(Line::from(right))
}
