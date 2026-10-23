//! The model picker's pure core (T4.3): a list modal modelled on the theme
//! picker that chooses one provider's model setting -- the provider default,
//! the discovered listed models, the current value when it is not among
//! them, and a "Custom..." entry for free text. Pure: no I/O and no `App`.
//! The picker never stores the catalog; the caller passes the live
//! `Option<&ProviderCatalog>` into every method, so a discovery update shows
//! at the next key, mouse event or render, and the selection is kept by
//! value so it stays on the same entry while models come and go. The
//! renderer lives in `ui::model_picker`; `App::open_model_picker` opens the
//! picker from the settings overlay's model rows and `App::finish_model_picker`
//! routes `Custom` to `SettingsOverlay::open_editor`.
//!
//! Any value stays possible (T4.5): `Custom` and the `Current` entry keep
//! values outside the discovered list reachable, because discovery is only
//! advice (warn-only) and the harness is the authority on which models it
//! accepts.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use patok_core::event::{ModelCatalogStatus, ProviderCatalog};
use ratatui::layout::{Position, Rect};

use crate::choice::choice_row_at;
use crate::hit::{close_clicked, footer_button_code};

/// The picker's bottom-line buttons: Enter selects the highlighted entry,
/// Esc closes with no effect. The renderer and the mouse hit-test both read
/// this one list.
pub(crate) const MODEL_PICKER_BUTTONS: [(&str, &str); 2] = [("Enter", "Select"), ("Esc", "Cancel")];

/// One row of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerEntry {
    /// The empty value: the provider's own default model.
    Default,
    /// A discovered, listed model.
    Model {
        id: String,
        display_name: Option<String>,
    },
    /// The current value, when it is non-empty and not a listed model.
    Current(String),
    /// Free text; the owner routes it to the inline editor.
    Custom,
}

impl PickerEntry {
    /// The setting value the entry stands for: `""` for the default, the
    /// model id or current value, `None` for the custom entry.
    pub(crate) fn value(&self) -> Option<&str> {
        match self {
            Self::Default => Some(""),
            Self::Model { id, .. } => Some(id),
            Self::Current(value) => Some(value),
            Self::Custom => None,
        }
    }

    /// What choosing the entry (Enter or a click) asks the owner to do.
    fn outcome(&self) -> PickerOutcome {
        match self.value() {
            Some(value) => PickerOutcome::Choose(value.to_owned()),
            None => PickerOutcome::Custom,
        }
    }
}

/// What a key or mouse event asks the owner to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PickerOutcome {
    /// Nothing beyond the picker's own state change.
    Stay,
    /// Close the picker with no effect.
    Close,
    /// Set the setting to this value (`""` is the provider default).
    Choose(String),
    /// Open the inline editor for a free-text value.
    Custom,
}

/// The stored selection: by value, so it survives catalog updates. The
/// default entry is `Value("")`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
    Value(String),
    Custom,
}

/// The picker's state: which provider it is for, the setting's current
/// value, the selection and the cells the renderer records for hit-testing.
pub(crate) struct ModelPicker {
    provider: String,
    current: String,
    selected: Selection,
    /// First visible row; the renderer writes it, the mouse hit-test reads it.
    pub offset: Cell<usize>,
    /// The entry rows' rect at the last render (the status note excluded).
    pub area: Cell<Rect>,
    /// The bottom line's rect at the last render.
    pub footer: Cell<Rect>,
    /// The close button's rect at the last render.
    pub close: Cell<Rect>,
}

impl ModelPicker {
    /// A picker for `provider` over the setting's `current` value; the
    /// selection starts on the entry for that value.
    pub(crate) fn new(provider: &str, current: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            current: current.to_owned(),
            selected: Selection::Value(current.to_owned()),
            offset: Cell::new(0),
            area: Cell::new(Rect::default()),
            footer: Cell::new(Rect::default()),
            close: Cell::new(Rect::default()),
        }
    }

    /// The provider slug the picker is for.
    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    /// The setting's current value (`""` for the provider default).
    pub(crate) fn current(&self) -> &str {
        &self.current
    }

    /// The entry list for the live catalog: the default, every listed model
    /// in catalog order (whatever the status, so a re-discovery still shows
    /// the last models; empty and duplicate ids skipped), the current value
    /// when it is non-empty and not listed, then the custom entry.
    pub(crate) fn entries(&self, catalog: Option<&ProviderCatalog>) -> Vec<PickerEntry> {
        let mut entries = vec![PickerEntry::Default];
        for model in catalog.into_iter().flat_map(|c| &c.models) {
            let seen = entries
                .iter()
                .any(|entry| entry.value() == Some(model.id.as_str()));
            if model.listed && !model.id.is_empty() && !seen {
                entries.push(PickerEntry::Model {
                    id: model.id.clone(),
                    display_name: model.display_name.clone(),
                });
            }
        }
        let listed = entries
            .iter()
            .any(|entry| entry.value() == Some(self.current.as_str()));
        if !listed {
            entries.push(PickerEntry::Current(self.current.clone()));
        }
        entries.push(PickerEntry::Custom);
        entries
    }

    /// The index of the selected entry. When the stored selection is no
    /// longer in the list (a model vanished in a live update) it falls back
    /// to the current value's entry, which always exists.
    pub(crate) fn selected_index(&self, entries: &[PickerEntry]) -> usize {
        let find = |selection: &Selection| {
            entries.iter().position(|entry| match selection {
                Selection::Custom => *entry == PickerEntry::Custom,
                Selection::Value(value) => entry.value() == Some(value.as_str()),
            })
        };
        find(&self.selected)
            .or_else(|| find(&Selection::Value(self.current.clone())))
            .unwrap_or(0)
    }

    /// The note shown above the entries when the catalog is not ready, or
    /// ready with nothing listed; `None` when there is nothing to say.
    pub(crate) fn status_note(catalog: Option<&ProviderCatalog>) -> Option<String> {
        let Some(catalog) = catalog else {
            return Some("Models not discovered yet".to_owned());
        };
        match catalog.status {
            ModelCatalogStatus::NotDiscovered => Some("Models not discovered yet".to_owned()),
            ModelCatalogStatus::Pending => Some("Discovering models...".to_owned()),
            ModelCatalogStatus::Failed if catalog.error.is_empty() => {
                Some("Model discovery failed".to_owned())
            }
            ModelCatalogStatus::Failed => {
                Some(format!("Model discovery failed: {}", catalog.error))
            }
            ModelCatalogStatus::Ready if !catalog.models.iter().any(|m| m.listed) => {
                Some("No models discovered".to_owned())
            }
            ModelCatalogStatus::Ready => None,
        }
    }

    /// Points the selection at `entry`.
    fn select(&mut self, entry: &PickerEntry) {
        self.selected = match entry.value() {
            Some(value) => Selection::Value(value.to_owned()),
            None => Selection::Custom,
        };
    }

    /// One key step: Up/`k` and Down/`j` move (clamped, no wrapping), Home
    /// and End jump, Enter chooses the selected entry, Esc closes, and
    /// everything else is swallowed. Matches the raw `KeyCode` with no
    /// modifier checks, like the shared choice list.
    pub(crate) fn on_key(
        &mut self,
        key: KeyEvent,
        catalog: Option<&ProviderCatalog>,
    ) -> PickerOutcome {
        let entries = self.entries(catalog);
        let index = self.selected_index(&entries);
        let last = entries.len() - 1;
        let target = match key.code {
            KeyCode::Up | KeyCode::Char('k') => index.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (index + 1).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            KeyCode::Enter => return entries[index].outcome(),
            KeyCode::Esc => return PickerOutcome::Close,
            _ => return PickerOutcome::Stay,
        };
        self.select(&entries[target]);
        PickerOutcome::Stay
    }

    /// One mouse step, in the order of the other list modals: the close
    /// button, the footer's buttons (which stand for their keys), then the
    /// entry rows -- a pointer move highlights a row, a left click chooses it.
    pub(crate) fn on_mouse(
        &mut self,
        mouse: MouseEvent,
        catalog: Option<&ProviderCatalog>,
    ) -> PickerOutcome {
        if close_clicked(&mouse, self.close.get()) {
            return PickerOutcome::Close;
        }
        if let Some(code) = footer_button_code(&mouse, self.footer.get(), &MODEL_PICKER_BUTTONS) {
            return self.on_key(KeyEvent::new(code, KeyModifiers::NONE), catalog);
        }
        let entries = self.entries(catalog);
        let area = self.area.get();
        let offset = self.offset.get();
        let visible = entries
            .len()
            .saturating_sub(offset)
            .min(usize::from(area.height));
        let position = Position::new(mouse.column, mouse.row);
        let Some(row) = choice_row_at(position, area, visible).map(|row| row + offset) else {
            return PickerOutcome::Stay;
        };
        match mouse.kind {
            MouseEventKind::Moved => {
                self.select(&entries[row]);
                PickerOutcome::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.select(&entries[row]);
                entries[row].outcome()
            }
            _ => PickerOutcome::Stay,
        }
    }
}

/// The first visible row that keeps `selected` in a `height`-row window over
/// `len` entries, moving from the previous offset by the least amount and
/// clamped so the window never runs past the end. 0 for an empty window.
pub(crate) fn scroll_offset(prev: usize, selected: usize, len: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    let offset = if selected < prev {
        selected
    } else if selected >= prev + height {
        selected + 1 - height
    } else {
        prev
    };
    offset.min(len.saturating_sub(height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit::footer_button_rects;
    use patok_core::event::CatalogModel;

    fn catalog(
        status: ModelCatalogStatus,
        models: &[(&str, Option<&str>, bool)],
    ) -> ProviderCatalog {
        ProviderCatalog {
            provider: "codex".to_owned(),
            status,
            error: String::new(),
            models: models
                .iter()
                .map(|(id, name, listed)| CatalogModel {
                    id: (*id).to_owned(),
                    display_name: name.map(str::to_owned),
                    listed: *listed,
                })
                .collect(),
        }
    }

    fn ready3() -> ProviderCatalog {
        catalog(
            ModelCatalogStatus::Ready,
            &[("a", Some("A"), true), ("b", None, true), ("c", None, true)],
        )
    }

    fn model(id: &str, name: Option<&str>) -> PickerEntry {
        PickerEntry::Model {
            id: id.to_owned(),
            display_name: name.map(str::to_owned),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(
        picker: &mut ModelPicker,
        code: KeyCode,
        c: Option<&ProviderCatalog>,
    ) -> PickerOutcome {
        picker.on_key(key(code), c)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn selected(picker: &ModelPicker, c: Option<&ProviderCatalog>) -> PickerEntry {
        let entries = picker.entries(c);
        entries[picker.selected_index(&entries)].clone()
    }

    #[test]
    fn entries_filter_unlisted_and_keep_order() {
        let c = catalog(
            ModelCatalogStatus::Ready,
            &[
                ("z", None, true),
                ("hidden", None, false),
                ("a", Some("A"), true),
            ],
        );
        let picker = ModelPicker::new("codex", "");
        assert_eq!(
            picker.entries(Some(&c)),
            vec![
                PickerEntry::Default,
                model("z", None),
                model("a", Some("A")),
                PickerEntry::Custom
            ]
        );
    }

    #[test]
    fn entries_current_variants() {
        let c = ready3();
        let empty = ModelPicker::new("codex", "").entries(Some(&c));
        assert!(!empty.iter().any(|e| matches!(e, PickerEntry::Current(_))));
        let listed = ModelPicker::new("codex", "b").entries(Some(&c));
        assert!(!listed.iter().any(|e| matches!(e, PickerEntry::Current(_))));
        let custom = ModelPicker::new("codex", "mine").entries(Some(&c));
        assert_eq!(
            custom[custom.len() - 2],
            PickerEntry::Current("mine".into())
        );
        assert_eq!(custom[custom.len() - 1], PickerEntry::Custom);
    }

    #[test]
    fn entries_skip_duplicate_and_empty_ids() {
        let c = catalog(
            ModelCatalogStatus::Ready,
            &[("a", None, true), ("a", None, true), ("", None, true)],
        );
        let entries = ModelPicker::new("codex", "").entries(Some(&c));
        assert_eq!(
            entries,
            vec![PickerEntry::Default, model("a", None), PickerEntry::Custom]
        );
    }

    #[test]
    fn entries_without_catalog() {
        assert_eq!(
            ModelPicker::new("codex", "").entries(None),
            vec![PickerEntry::Default, PickerEntry::Custom]
        );
        assert_eq!(
            ModelPicker::new("codex", "x").entries(None),
            vec![
                PickerEntry::Default,
                PickerEntry::Current("x".into()),
                PickerEntry::Custom
            ]
        );
    }

    #[test]
    fn status_notes() {
        let note = ModelPicker::status_note;
        let with = |status| catalog(status, &[("a", None, true)]);
        assert_eq!(note(None).as_deref(), Some("Models not discovered yet"));
        assert_eq!(
            note(Some(&with(ModelCatalogStatus::NotDiscovered))).as_deref(),
            Some("Models not discovered yet")
        );
        assert_eq!(
            note(Some(&with(ModelCatalogStatus::Pending))).as_deref(),
            Some("Discovering models...")
        );
        let mut failed = with(ModelCatalogStatus::Failed);
        assert_eq!(
            note(Some(&failed)).as_deref(),
            Some("Model discovery failed")
        );
        failed.error = "boom".into();
        assert_eq!(
            note(Some(&failed)).as_deref(),
            Some("Model discovery failed: boom")
        );
        assert_eq!(
            note(Some(&catalog(
                ModelCatalogStatus::Ready,
                &[("h", None, false)]
            )))
            .as_deref(),
            Some("No models discovered")
        );
        assert_eq!(note(Some(&with(ModelCatalogStatus::Ready))), None);
    }

    #[test]
    fn new_selects_the_current_value() {
        let c = ready3();
        assert_eq!(
            selected(&ModelPicker::new("codex", ""), Some(&c)),
            PickerEntry::Default
        );
        assert_eq!(
            selected(&ModelPicker::new("codex", "b"), Some(&c)),
            model("b", None)
        );
        assert_eq!(
            selected(&ModelPicker::new("codex", "mine"), Some(&c)),
            PickerEntry::Current("mine".into())
        );
    }

    #[test]
    fn movement_is_clamped_and_jk_aliases() {
        let c = ready3();
        let mut p = ModelPicker::new("codex", "");
        press(&mut p, KeyCode::Up, Some(&c));
        assert_eq!(selected(&p, Some(&c)), PickerEntry::Default);
        press(&mut p, KeyCode::Down, Some(&c));
        assert_eq!(selected(&p, Some(&c)), model("a", Some("A")));
        press(&mut p, KeyCode::Char('j'), Some(&c));
        assert_eq!(selected(&p, Some(&c)), model("b", None));
        press(&mut p, KeyCode::Char('k'), Some(&c));
        assert_eq!(selected(&p, Some(&c)), model("a", Some("A")));
        press(&mut p, KeyCode::End, Some(&c));
        assert_eq!(selected(&p, Some(&c)), PickerEntry::Custom);
        press(&mut p, KeyCode::Down, Some(&c));
        assert_eq!(selected(&p, Some(&c)), PickerEntry::Custom);
        press(&mut p, KeyCode::Home, Some(&c));
        assert_eq!(selected(&p, Some(&c)), PickerEntry::Default);
    }

    #[test]
    fn enter_outcomes() {
        let c = ready3();
        let mut p = ModelPicker::new("codex", "mine");
        assert_eq!(
            press(&mut p, KeyCode::Enter, Some(&c)),
            PickerOutcome::Choose("mine".into())
        );
        press(&mut p, KeyCode::Home, Some(&c));
        assert_eq!(
            press(&mut p, KeyCode::Enter, Some(&c)),
            PickerOutcome::Choose(String::new())
        );
        press(&mut p, KeyCode::Down, Some(&c));
        assert_eq!(
            press(&mut p, KeyCode::Enter, Some(&c)),
            PickerOutcome::Choose("a".into())
        );
        press(&mut p, KeyCode::End, Some(&c));
        assert_eq!(
            press(&mut p, KeyCode::Enter, Some(&c)),
            PickerOutcome::Custom
        );
    }

    #[test]
    fn esc_closes_and_other_keys_stay() {
        let c = ready3();
        let mut p = ModelPicker::new("codex", "b");
        assert_eq!(press(&mut p, KeyCode::Esc, Some(&c)), PickerOutcome::Close);
        assert_eq!(
            press(&mut p, KeyCode::Char('x'), Some(&c)),
            PickerOutcome::Stay
        );
        assert_eq!(selected(&p, Some(&c)), model("b", None));
    }

    #[test]
    fn selection_survives_a_catalog_arriving() {
        let mut p = ModelPicker::new("codex", "");
        press(&mut p, KeyCode::Down, None);
        assert_eq!(selected(&p, None), PickerEntry::Custom);
        let c = ready3();
        assert_eq!(
            press(&mut p, KeyCode::Enter, Some(&c)),
            PickerOutcome::Custom
        );
    }

    #[test]
    fn vanished_selection_falls_back_to_current() {
        let c = ready3();
        let mut p = ModelPicker::new("codex", "mine");
        press(&mut p, KeyCode::Home, Some(&c));
        press(&mut p, KeyCode::Down, Some(&c));
        assert_eq!(selected(&p, Some(&c)), model("a", Some("A")));
        let gone = catalog(ModelCatalogStatus::Ready, &[("b", None, true)]);
        assert_eq!(
            selected(&p, Some(&gone)),
            PickerEntry::Current("mine".into())
        );
    }

    fn placed() -> ModelPicker {
        let p = ModelPicker::new("codex", "");
        p.area.set(Rect::new(10, 5, 40, 3));
        p.footer.set(Rect::new(10, 9, 40, 1));
        p.close.set(Rect::new(40, 4, 7, 1));
        p
    }

    #[test]
    fn mouse_hover_and_click() {
        let c = ready3();
        let mut p = placed();
        assert_eq!(
            p.on_mouse(mouse(MouseEventKind::Moved, 12, 6), Some(&c)),
            PickerOutcome::Stay
        );
        assert_eq!(selected(&p, Some(&c)), model("a", Some("A")));
        assert_eq!(
            p.on_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 12, 7),
                Some(&c)
            ),
            PickerOutcome::Choose("b".into())
        );
        assert_eq!(
            p.on_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 12, 8),
                Some(&c)
            ),
            PickerOutcome::Stay,
        );
    }

    #[test]
    fn mouse_close_and_footer_buttons() {
        let c = ready3();
        let mut p = placed();
        assert_eq!(
            p.on_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 41, 4),
                Some(&c)
            ),
            PickerOutcome::Close
        );
        let rects = footer_button_rects(p.footer.get(), &MODEL_PICKER_BUTTONS);
        let click = |r: Rect| mouse(MouseEventKind::Down(MouseButton::Left), r.x, r.y);
        assert_eq!(p.on_mouse(click(rects[1]), Some(&c)), PickerOutcome::Close);
        assert_eq!(
            p.on_mouse(click(rects[0]), Some(&c)),
            PickerOutcome::Choose(String::new())
        );
    }

    #[test]
    fn mouse_click_below_the_last_row_stays() {
        let mut p = placed();
        p.area.set(Rect::new(10, 5, 40, 10));
        // No catalog: two entries, rows 5 and 6.
        assert_eq!(
            p.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 12, 8), None),
            PickerOutcome::Stay
        );
    }

    #[test]
    fn mouse_respects_the_scroll_offset() {
        let c = ready3();
        let mut p = placed();
        p.offset.set(2);
        p.on_mouse(mouse(MouseEventKind::Moved, 12, 5), Some(&c));
        assert_eq!(selected(&p, Some(&c)), model("b", None));
    }

    #[test]
    fn scroll_offset_cases() {
        assert_eq!(scroll_offset(0, 2, 10, 5), 0);
        assert_eq!(scroll_offset(0, 7, 10, 5), 3);
        assert_eq!(scroll_offset(6, 2, 10, 5), 2);
        assert_eq!(scroll_offset(0, 9, 10, 5), 5);
        assert_eq!(scroll_offset(4, 3, 3, 5), 0);
        assert_eq!(scroll_offset(3, 5, 10, 0), 0);
    }
}
