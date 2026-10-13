//! The theme picker's keys (finding 19, the rest, in
//! docs/private/structure-review-followup.md): opening, browsing,
//! previewing and committing the picker's selection, split out of `app`;
//! the picker's state lives on `App` and its rendering in `ui::dialogs`.

use patok_core::config::Theme as ThemeKey;

use crate::app::{Action, App};
use crate::modal::Modal;
use crate::overlay::{Entry, clamp_focus};

impl App {
    /// Opens the theme picker (the `t` key, T43.1): the selection starts on
    /// the active theme, and the theme active now is remembered, so Esc
    /// restores it exactly. Both groups open expanded (T116.1), the fold state
    /// resetting with every open. Nothing is previewed or persisted yet.
    /// A refused push (another modal already open) is a no-op: the picker's
    /// state is left exactly as it was.
    pub(crate) fn open_theme(&mut self) {
        if !self.modals.push(Modal::Theme) {
            return;
        }
        self.theme_modal.original = self.tui.theme;
        self.theme_modal.expanded = [true, true];
        let active = self.tui.theme.as_str();
        self.theme_modal.selected = self
            .theme_modal
            .entries()
            .iter()
            .position(|entry| matches!(entry, Entry::Row(name) if *name == active))
            .unwrap_or(0);
        self.theme_modal.preview = None;
    }

    /// Moves the selection one visible entry (`delta` sign) and previews the
    /// entry it lands on when it is a row; a header takes only the highlight.
    /// The visible list contains no entry of a collapsed group, so collapsed
    /// entries are skipped by construction; the clamp keeps both ends put.
    pub(crate) fn move_selection(&mut self, delta: isize) {
        let len = self.theme_modal.entries().len();
        let target = clamp_focus(self.theme_modal.selected.saturating_add_signed(delta), len);
        if let Entry::Row(name) = self.theme_modal.entries()[target] {
            self.theme_modal.preview =
                Some(ThemeKey::parse(name).unwrap_or(self.theme_modal.original));
        }
        self.theme_modal.selected = target;
    }

    /// The theme of the selected row, when the selection is on one: a
    /// built-in name, always parseable. On a header there is no row theme, so
    /// the remembered original serves as the same safety fallback a corrupt
    /// list could never hit.
    fn selected_theme(&self) -> ThemeKey {
        match self.theme_modal.selected_entry() {
            Entry::Row(name) => ThemeKey::parse(name).unwrap_or(self.theme_modal.original),
            Entry::Header(_) => self.theme_modal.original,
        }
    }

    /// Points the selection at visible entry `index` and immediately previews
    /// that theme when it is a row: the whole shell recolours on the next
    /// render with nothing persisted and no engine round trip, since the
    /// theme is a tui-schema field. A header takes only the highlight; the
    /// preview stays what it was.
    pub(crate) fn preview_entry(&mut self, index: usize) {
        let len = self.theme_modal.entries().len();
        self.theme_modal.selected = clamp_focus(index, len);
        self.theme_modal.preview = match self.theme_modal.selected_entry() {
            Entry::Row(name) => Some(ThemeKey::parse(name).unwrap_or(self.theme_modal.original)),
            Entry::Header(_) => self.theme_modal.preview,
        };
    }

    /// The Enter path: keep the currently previewed theme active, close the
    /// picker, and ask the driver to persist the same value through the tui
    /// field's read-modify-write. On a failed save the driver restores the
    /// remembered original.
    pub(crate) fn commit_theme(&mut self) -> Action {
        let theme = self.selected_theme();
        self.theme_modal.preview = None;
        self.modals.pop(Modal::Theme);
        self.tui.theme = theme;
        Action::SaveTheme(theme)
    }

    /// The theme choice was persisted (T43.1): the status bar confirms it.
    pub fn on_theme_saved(&mut self) {
        self.status = Some("Theme saved.".into());
    }

    /// Persisting the theme choice failed: the theme the picker opened with is
    /// restored, so the shell matches the config file that is still on disk.
    pub fn on_theme_save_failed(&mut self, error: String) {
        self.tui.theme = self.theme_modal.original;
        self.status = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_core::event::{Phase, Snapshot};
    use patok_core::pipeline::PipelineState;

    use crate::theme::theme_modal_groups;

    fn app() -> App {
        App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
                current_task: None,
                planning: false,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                recent: vec![],
            },
            "0.1.0".into(),
        )
    }

    fn opened() -> App {
        let mut app = app();
        app.open_theme();
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn entries_of(app: &App) -> Vec<Entry<&'static str>> {
        app.theme_modal.entries()
    }

    /// The picker's structure (T116.1): with both groups expanded, the visible
    /// list is the Dark header, the dark themes, the Light header and the
    /// light themes, in `theme_modal_groups` order.
    #[test]
    fn the_picker_lists_two_groups_with_all_entries_visible() {
        let app = opened();
        let groups = theme_modal_groups();
        let mut expected = vec![Entry::Header(0)];
        expected.extend(groups[0].1.iter().map(|name| Entry::Row(*name)));
        expected.push(Entry::Header(1));
        expected.extend(groups[1].1.iter().map(|name| Entry::Row(*name)));
        assert_eq!(expected.len(), 13, "two headers plus the eleven built-ins");
        assert_eq!(entries_of(&app), expected);
        assert_eq!(entries_of(&app)[0], Entry::Header(0));
        assert_eq!(entries_of(&app)[7], Entry::Header(1));
        assert_eq!(app.theme_modal.expanded, [true, true]);
    }

    /// Toggling a header hides and shows its entries, and the selection stays
    /// on a visible entry (clamped into the shortened list).
    #[test]
    fn toggling_a_header_hides_and_shows_its_entries() {
        let mut app = opened();
        let is_dark = |entry: &Entry<&'static str>| match entry {
            Entry::Row(name) => ThemeKey::parse(name).is_some_and(ThemeKey::is_dark),
            Entry::Header(_) => false,
        };

        // Folding Dark leaves its header, the Light header and the light rows.
        app.theme_modal.toggle(0);
        let entries = entries_of(&app);
        assert_eq!(entries.len(), 7);
        assert!(!entries.iter().any(is_dark), "no dark row stays visible");
        assert_eq!(entries[0], Entry::Header(0));
        assert_eq!(entries[1], Entry::Header(1));
        // Unfolding brings every dark row back.
        app.theme_modal.toggle(0);
        assert_eq!(entries_of(&app).len(), 13);

        // Folding Light is symmetric: the two headers and the dark rows.
        app.theme_modal.toggle(1);
        let entries = entries_of(&app);
        assert_eq!(entries.len(), 8);
        assert_eq!(entries[7], Entry::Header(1));
        assert!(entries[1..7].iter().all(is_dark));
        app.theme_modal.toggle(1);
        assert_eq!(entries_of(&app).len(), 13);

        // A selection past the shortened list clamps onto the last entry.
        app.theme_modal.selected = 12;
        app.theme_modal.toggle(0);
        assert_eq!(app.theme_modal.selected, 6);
        app.theme_modal.toggle(0);
        assert_eq!(entries_of(&app).len(), 13);
    }

    /// Navigation walks the visible list only: entries of a collapsed group
    /// are never landed on, and moves that would leave the list clamp.
    #[test]
    fn collapsed_entries_are_skipped_during_navigation() {
        let mut app = opened();
        assert_eq!(app.theme_modal.selected, 1, "the active theme's row");

        // Fold Dark from its header.
        assert_eq!(press(&mut app, KeyCode::Up), Action::None);
        assert_eq!(app.theme_modal.selected, 0);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.theme_modal.expanded, [false, true]);

        // Down from the Dark header lands on the Light header -- never on a
        // hidden dark row -- and the preview is untouched on a header.
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(app.theme_modal.selected, 1);
        assert_eq!(app.theme_modal.preview, None);
        // The next Down lands on the first light row and previews it.
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(app.theme_modal.selected, 2);
        assert_eq!(app.theme_modal.preview, Some(ThemeKey::AtomOneLight));
        // Up from a light row lands back on the Light header, not a dark row.
        assert_eq!(press(&mut app, KeyCode::Up), Action::None);
        assert_eq!(app.theme_modal.selected, 1);
        assert_eq!(app.theme_modal.preview, Some(ThemeKey::AtomOneLight));

        // With both groups folded, navigation moves only between the headers
        // and clamps at both ends.
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.theme_modal.expanded, [false, false]);
        for _ in 0..3 {
            assert_eq!(press(&mut app, KeyCode::Down), Action::None);
            assert!(app.theme_modal.selected <= 1);
        }
        assert_eq!(app.theme_modal.selected, 1);
        for _ in 0..3 {
            assert_eq!(press(&mut app, KeyCode::Up), Action::None);
            assert!(app.theme_modal.selected <= 1);
        }
        assert_eq!(app.theme_modal.selected, 0);
    }
}
