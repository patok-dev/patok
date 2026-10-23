//! The model picker's renderer (T4.3): the one `Frame`-touching half of the
//! model picker; the pure core -- entries, status note, selection, key and
//! mouse handling -- lives in [`crate::model_picker`]. The renderer records
//! the rects the picker's mouse hit-testing reads at the next event.

use patok_core::event::ProviderCatalog;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::common::{base_style, render_modal_footer, selected_row_style};
use super::modal::{ModalTitle, render_modal_frame};
use crate::model_picker::{MODEL_PICKER_BUTTONS, ModelPicker, PickerEntry, scroll_offset};
use crate::theme::Theme;

/// The model picker's rect (T4.3): a centered modal, at least 54 columns
/// wide and as tall as its entry rows, the optional status note, the border
/// (2) and the footer (1) need, kept between 7 and 18 rows and clamped to
/// the screen.
pub(crate) fn model_picker_area(screen: Rect, rows: usize, note: bool) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let height = (rows + usize::from(note) + 3).clamp(7, 18);
    let height = u16::try_from(height).unwrap_or(18).min(screen.height);
    crate::geom::centered(screen, width, height)
}

/// The model picker: a centered " Model · provider " modal with the status
/// note (when the catalog is not ready) above a flat list of entries, the
/// selected one marked and in the shared selected-row style, and the usual
/// two-zone bottom line. The entries and note come from the live `catalog`
/// on every render, so discovery updates show at once. Records the close,
/// rows, footer and scroll offset in the picker for its mouse hit-testing.
pub(crate) fn render_model_picker(
    frame: &mut Frame,
    theme: Theme,
    picker: &ModelPicker,
    catalog: Option<&ProviderCatalog>,
) {
    let entries = picker.entries(catalog);
    let note = ModelPicker::status_note(catalog);
    let selected = picker.selected_index(&entries);
    let area = model_picker_area(frame.area(), entries.len(), note.is_some());
    let title = format!(" Model · {} ", picker.provider());
    let chrome = render_modal_frame(frame, theme, area, ModalTitle::Centered(&title));
    picker.close.set(chrome.close);
    let [note_area, rows, footer] = Layout::vertical([
        Constraint::Length(u16::from(note.is_some())),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(chrome.inner);
    if let Some(note) = note {
        frame.render_widget(
            Paragraph::new(format!(" {note}")).style(Style::new().fg(theme.muted_text)),
            note_area,
        );
    }
    picker.area.set(rows);
    let height = usize::from(rows.height);
    let offset = scroll_offset(picker.offset.get(), selected, entries.len(), height);
    picker.offset.set(offset);
    let width = entries
        .iter()
        .map(|e| label(e).chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(index, entry)| entry_line(theme, picker, entry, index == selected, width))
        .collect();
    frame.render_widget(Paragraph::new(lines), rows);
    picker.footer.set(footer);
    render_modal_footer(frame, footer, &["↑↓/jk move"], &MODEL_PICKER_BUTTONS, theme);
}

/// The entry's label: the model id, the current value, or the fixed names of
/// the default and custom entries.
fn label(entry: &PickerEntry) -> &str {
    match entry {
        PickerEntry::Default => "Provider default",
        PickerEntry::Model { id, .. } => id,
        PickerEntry::Current(value) => value,
        PickerEntry::Custom => "Custom...",
    }
}

/// One entry line, mirroring the theme picker's row: the marker, the label
/// padded to `width`, then a muted suffix -- the display name of a model
/// whose name differs from its id, and `(current)` on the entry standing for
/// the setting's current value. The selected line wears the shared
/// selected-row style, its suffix included.
fn entry_line(
    theme: Theme,
    picker: &ModelPicker,
    entry: &PickerEntry,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let marker = if selected { "▶ " } else { "  " };
    let (fg, suffix_fg) = if selected {
        (theme.highlighted_text, theme.highlighted_text)
    } else {
        (theme.normal_text, theme.muted_text)
    };
    let mut suffix = String::new();
    if let PickerEntry::Model {
        id,
        display_name: Some(name),
    } = entry
        && name != id
    {
        suffix.push_str(&format!("  {name}"));
    }
    if entry.value() == Some(picker.current()) {
        suffix.push_str("  (current)");
    }
    let mut spans = vec![
        Span::styled(marker, Style::new().fg(fg)),
        Span::styled(format!("{:<width$}", label(entry)), Style::new().fg(fg)),
    ];
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::new().fg(suffix_fg)));
    }
    let mut line = Line::from(spans).style(base_style(theme));
    if selected {
        line = line.style(selected_row_style(theme));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use patok_core::event::{CatalogModel, ModelCatalogStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn catalog(status: ModelCatalogStatus, ids: &[(&str, Option<&str>)]) -> ProviderCatalog {
        ProviderCatalog {
            provider: "codex".to_owned(),
            status,
            error: String::new(),
            models: ids
                .iter()
                .map(|(id, name)| CatalogModel {
                    id: (*id).to_owned(),
                    display_name: name.map(str::to_owned),
                    listed: true,
                })
                .collect(),
        }
    }

    fn codex_ready() -> ProviderCatalog {
        catalog(
            ModelCatalogStatus::Ready,
            &[
                ("gpt-5.5", Some("GPT-5.5")),
                ("gpt-5.4-mini", Some("GPT-5.4 Mini")),
                ("codex-auto", Some("codex-auto")),
            ],
        )
    }

    fn draw(picker: &ModelPicker, c: Option<&ProviderCatalog>, w: u16, h: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render_model_picker(frame, Theme::DARK, picker, c))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn snap(buffer: &Buffer) -> String {
        let area = buffer.area;
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn ready_with_custom_current() {
        let c = codex_ready();
        let picker = ModelPicker::new("codex", "my-model");
        insta::assert_snapshot!(snap(&draw(&picker, Some(&c), 80, 24)));
    }

    #[test]
    fn ready_listed_current() {
        let c = codex_ready();
        let picker = ModelPicker::new("codex", "gpt-5.4-mini");
        insta::assert_snapshot!(snap(&draw(&picker, Some(&c), 80, 24)));
    }

    #[test]
    fn pending_note() {
        let c = catalog(ModelCatalogStatus::Pending, &[]);
        let picker = ModelPicker::new("codex", "");
        insta::assert_snapshot!(snap(&draw(&picker, Some(&c), 80, 24)));
    }

    #[test]
    fn failed_note() {
        let mut c = catalog(ModelCatalogStatus::Failed, &[]);
        c.error = "codex exited with status 1".to_owned();
        let picker = ModelPicker::new("codex", "");
        insta::assert_snapshot!(snap(&draw(&picker, Some(&c), 80, 24)));
    }

    #[test]
    fn not_discovered() {
        let picker = ModelPicker::new("codex", "");
        insta::assert_snapshot!(snap(&draw(&picker, None, 80, 24)));
    }

    #[test]
    fn scrolled_long_list() {
        let ids: Vec<String> = (1..=30).map(|n| format!("model-{n:02}")).collect();
        let pairs: Vec<(&str, Option<&str>)> = ids.iter().map(|id| (id.as_str(), None)).collect();
        let c = catalog(ModelCatalogStatus::Ready, &pairs);
        let mut picker = ModelPicker::new("codex", "");
        picker.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), Some(&c));
        insta::assert_snapshot!(snap(&draw(&picker, Some(&c), 80, 16)));
    }

    #[test]
    fn records_rects_inside_the_modal() {
        let c = codex_ready();
        let picker = ModelPicker::new("codex", "");
        draw(&picker, Some(&c), 80, 24);
        let modal = model_picker_area(Rect::new(0, 0, 80, 24), 5, false);
        for rect in [picker.area.get(), picker.footer.get(), picker.close.get()] {
            assert!(rect.width > 0 && rect.height > 0);
            assert!(modal.contains(rect.as_position()));
        }
    }

    #[test]
    fn hover_round_trips_through_the_recorded_rows() {
        let c = codex_ready();
        let mut picker = ModelPicker::new("codex", "");
        draw(&picker, Some(&c), 80, 24);
        let rows = picker.area.get();
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            column: rows.x + 3,
            row: rows.y + 2,
            modifiers: KeyModifiers::NONE,
        };
        picker.on_mouse(moved, Some(&c));
        let entries = picker.entries(Some(&c));
        assert_eq!(picker.selected_index(&entries), 2);
    }

    #[test]
    fn selected_label_wears_highlighted_text() {
        let c = codex_ready();
        let picker = ModelPicker::new("codex", "");
        let buffer = draw(&picker, Some(&c), 80, 24);
        let rows = picker.area.get();
        assert_eq!(
            buffer[(rows.x + 2, rows.y)].fg,
            Theme::DARK.highlighted_text
        );
        assert_eq!(buffer[(rows.x + 2, rows.y + 1)].fg, Theme::DARK.normal_text);
    }

    #[test]
    fn area_clamps_to_a_small_screen() {
        let area = model_picker_area(Rect::new(0, 0, 40, 6), 30, true);
        assert_eq!((area.width, area.height), (40, 6));
        let tall = model_picker_area(Rect::new(0, 0, 100, 50), 30, true);
        assert_eq!(tall.height, 18);
    }
}
