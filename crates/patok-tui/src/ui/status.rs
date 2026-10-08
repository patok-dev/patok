//! The merged bottom status line.

use patok_core::event::Phase;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::common::{button_line, button_text};
use crate::app::App;

/// The merged status line (T86.1): one bottom row holding the old header's
/// chips on the left -- the status chip, then the run-mode chip when the two
/// fit -- and the status content on the right: a transient message, else the
/// key-hint buttons, right-aligned. Hint buttons drop as whole buttons from
/// the tail when they do not fit; a message is never dropped -- it truncates
/// at the line's right edge like a bare status bar message. The secondary
/// hints (settings, theme, detach, quit) live behind the m menu, so the zone
/// carries the idle Enter button, the running Esc stop button (T134.1) and
/// the m menu button. All three are interactive (T133.1, T134.1): each renders
/// through the shared [`button_line`] so it reads as a modal button, and each
/// records a rect covering its full [`button_text`] width on
/// [`App::menu_chip`], [`App::enter_chip`] or [`App::stop_chip`] for the mouse
/// hit-test -- a left click anywhere on the button runs its key's action.
pub(super) fn status_widget(app: &App, area: Rect) -> Paragraph<'static> {
    let width = usize::from(area.width);
    let theme = app.theme();
    // A discovery round runs either on an idle engine or inside a session's empty-queue gap
    // (the phase stays Running for the whole session), so it wins over the phase chips.
    let (label, color) = match (app.stopping, app.phase) {
        (true, _) => ("STOPPING", theme.highlighted_text),
        (false, _) if app.planning => ("PLANNING", theme.chip_planning),
        (false, _) if app.discovering => ("DISCOVERING", theme.chip_discovering),
        (false, Phase::Running) => ("RUNNING", theme.success),
        (false, Phase::Startup) => ("STOPPED", theme.chip_neutral),
    };
    let chip = format!(" {label} ");
    // The run-mode chip: a quieter second chip mirroring the engine-reported readout,
    // so a change applied through the settings overlay flips it immediately.
    let mode = format!(" {} ", app.run_mode());

    // The run-mode chip drops when the two do not fit; the status chip survives.
    let fits_mode = chip.chars().count() + mode.chars().count() <= width;
    let mut spans = vec![Span::styled(
        chip,
        Style::new().fg(theme.contrast_text).bg(color).bold(),
    )];
    if fits_mode {
        spans.push(Span::styled(
            mode,
            Style::new().fg(theme.contrast_text).bg(theme.chip_neutral),
        ));
    }
    let left_width: usize = spans.iter().map(|span| span.content.chars().count()).sum();

    // The interactive chips' rects reset every render: a showing message or
    // a pair dropped for width leaves the zero rect, which contains no real
    // position, so a click misses naturally.
    app.menu_chip.set(Rect::default());
    app.enter_chip.set(Rect::default());
    app.stop_chip.set(Rect::default());
    let right = match &app.status {
        Some(message) => {
            // Bare text: one separating space so it does not glue to the mode
            // chip, right-aligned padding when it fits -- none when it would
            // underflow, so the Paragraph truncates at the right edge.
            let pad = width.saturating_sub(left_width + 1 + message.chars().count());
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
                    if app.tasks.iter().any(|t| !t.done) {
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
            if (app.phase == Phase::Running || app.planning || app.discovering) && !app.stopping {
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
            // added.
            let zone = width.saturating_sub(left_width);
            let mut kept: Vec<(&str, &str)> = Vec::new();
            let mut used = 0;
            for (key, label) in keys {
                let pair = button_text(key, label).chars().count();
                if used + pair > zone {
                    break;
                }
                used += pair;
                kept.push((key, label));
            }
            let mut right = vec![Span::raw(" ".repeat(zone.saturating_sub(used)))];
            // The hint zone's running column: the pad ends where the first
            // kept button starts, and each button advances it by its own
            // character count, so every interactive chip's rect is the exact
            // columns its full button text occupies.
            let mut column = left_width + zone.saturating_sub(used);
            for (key, label) in kept {
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
                    "m" => app.menu_chip.set(rect),
                    "Enter" => app.enter_chip.set(rect),
                    _ => app.stop_chip.set(rect),
                }
                right.extend(button_line(key, label, theme).spans);
                column += usize::from(width);
            }
            right
        }
    };
    spans.extend(right);
    Paragraph::new(Line::from(spans))
}
