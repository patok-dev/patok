//! The settings overlay: a static row model over the
//! configuration that exists after T13.1 and T14.1, plus the pure interaction state --
//! focus, collapsible sections, the inline editor, the unsaved-changes dialog, the
//! draft map and the status line. Editing a value only updates the draft map in this
//! state; nothing is applied or persisted until the user picks save in the close
//! dialog, and the driver (`run.rs`) then runs every drafted change through the
//! settings-change flow (daemon-schema fields via the engine, tui-schema fields
//! directly by the shell). The help box (T85.1) carries no text of its own: it
//! routes the focused entry through the owning schema's registry in `patok-core`.

use std::cell::Cell;
use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use patok_core::config::{SettingValue, THEME_KEYS, daemon_help, tui_help};
use ratatui::layout::Rect;

use crate::app::Action;

/// Which schema a field belongs to, and so who applies a change to it: daemon fields go through the engine, tui fields are the shell's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schema {
    Daemon,
    Tui,
}

/// A numeric field's accepted shape (checked inline before a change is submitted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Number {
    /// A whole number, at most `u32::MAX` (the wire cases carry u32).
    Uint,
    /// A whole number of seconds greater than zero, at most `u32::MAX`.
    PositiveUint,
    /// A number in 0.0..=1.0.
    Float,
}

/// A row's field kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// A report line; no editor.
    ReadOnly,
    /// A checkbox toggled with Enter or Space.
    Bool,
    /// An enumerated field cycled with Left, Right or Enter; the accepted spellings in
    /// cycle order.
    Enum(&'static [&'static str]),
    /// A number edited inline with Enter to save and Ctrl+U to clear.
    Number(Number),
    /// Free text edited inline; the empty string clears an optional field.
    Text,
}

/// One editable (or read-only) field of the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub key: &'static str,
    pub label: &'static str,
    pub schema: Schema,
    pub kind: FieldKind,
}

/// One collapsible section and its rows. The `group` id is the key the help
/// box (T85.1) looks the section's description up by in the owning schema's
/// registry -- stable and independent of the display `title`.
pub struct Section {
    pub title: &'static str,
    /// The section's fold state when the overlay opens.
    pub expanded: bool,
    /// The group id the section's help text is registered under (T85.1).
    pub group: &'static str,
    pub rows: &'static [Row],
}

// Deliberate exclusions from the row model: per-stage model pickers (no model catalog
// exists yet, so there is nothing to pick from), list- and map-valued fields (`stages`,
// `stage_overrides`, `catalog_url_overrides`, `semgrep_rulesets`, `plugins` -- composite,
// no single-value editor) and `agent_pane_split` (the TUI has a fixed split and no
// dragging). Everything else the registries accept appears below.

const PROVIDERS: &[&str] = &["claude", "codex", "opencode", "ghcopilot", "mistral"];

static PROVIDER_SECTION: &[Row] = &[
    Row {
        key: "engine_version",
        label: "engine version",
        schema: Schema::Daemon,
        kind: FieldKind::ReadOnly,
    },
    Row {
        key: "shell_version",
        label: "shell version",
        schema: Schema::Daemon,
        kind: FieldKind::ReadOnly,
    },
    Row {
        key: "provider",
        label: "provider",
        schema: Schema::Daemon,
        kind: FieldKind::Enum(PROVIDERS),
    },
    Row {
        key: "model",
        label: "model",
        schema: Schema::Daemon,
        kind: FieldKind::Text,
    },
    Row {
        key: "research_provider",
        label: "research provider",
        schema: Schema::Daemon,
        kind: FieldKind::Enum(PROVIDERS),
    },
    Row {
        key: "research_model",
        label: "research model",
        schema: Schema::Daemon,
        kind: FieldKind::Text,
    },
    Row {
        key: "planner_provider",
        label: "planner provider",
        schema: Schema::Daemon,
        kind: FieldKind::Enum(PROVIDERS),
    },
    Row {
        key: "planner_model",
        label: "planner model",
        schema: Schema::Daemon,
        kind: FieldKind::Text,
    },
    Row {
        key: "builder_provider",
        label: "builder provider",
        schema: Schema::Daemon,
        kind: FieldKind::Enum(PROVIDERS),
    },
    Row {
        key: "builder_model",
        label: "builder model",
        schema: Schema::Daemon,
        kind: FieldKind::Text,
    },
    Row {
        key: "reviewer_provider",
        label: "reviewer provider",
        schema: Schema::Daemon,
        kind: FieldKind::Enum(PROVIDERS),
    },
    Row {
        key: "reviewer_model",
        label: "reviewer model",
        schema: Schema::Daemon,
        kind: FieldKind::Text,
    },
    Row {
        key: "discovery_provider",
        label: "discovery provider",
        schema: Schema::Daemon,
        kind: FieldKind::Enum(PROVIDERS),
    },
    Row {
        key: "discovery_model",
        label: "discovery model",
        schema: Schema::Daemon,
        kind: FieldKind::Text,
    },
];

static PIPELINE_SECTION: &[Row] = &[
    Row {
        key: "run_mode",
        label: "run mode",
        schema: Schema::Daemon,
        kind: FieldKind::Enum(&["sprint", "continuous"]),
    },
    Row {
        key: "plan_enabled",
        label: "plan enabled",
        schema: Schema::Daemon,
        kind: FieldKind::Bool,
    },
    Row {
        key: "skip_planner_for_simple",
        label: "skip planner for simple",
        schema: Schema::Daemon,
        kind: FieldKind::Bool,
    },
    Row {
        key: "skip_research_for_simple",
        label: "skip research for simple",
        schema: Schema::Daemon,
        kind: FieldKind::Bool,
    },
    Row {
        key: "skip_review_for_simple",
        label: "skip review for simple",
        schema: Schema::Daemon,
        kind: FieldKind::Bool,
    },
    Row {
        key: "review_confidence_threshold",
        label: "review confidence threshold",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::Uint),
    },
    Row {
        key: "review_multipass_threshold",
        label: "review multipass threshold",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::Uint),
    },
    Row {
        key: "batch_review",
        label: "batch review",
        schema: Schema::Daemon,
        kind: FieldKind::Bool,
    },
    Row {
        key: "planner_lookahead",
        label: "planner lookahead",
        schema: Schema::Daemon,
        kind: FieldKind::Bool,
    },
    Row {
        key: "review_in_loop",
        label: "review in loop",
        schema: Schema::Daemon,
        kind: FieldKind::Bool,
    },
    Row {
        key: "confidence_threshold",
        label: "confidence threshold",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::Float),
    },
];

static TIMEOUTS_SECTION: &[Row] = &[
    Row {
        key: "agent_timeout_secs",
        label: "agent timeout",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::PositiveUint),
    },
    Row {
        key: "pause_between_tasks_secs",
        label: "pause between tasks",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::Uint),
    },
    Row {
        key: "pause_between_agents_secs",
        label: "pause between agents",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::Uint),
    },
    Row {
        key: "pause_between_cycles_secs",
        label: "pause between cycles",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::Uint),
    },
    Row {
        key: "engine_idle_timeout_secs",
        label: "engine idle timeout",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::Uint),
    },
    Row {
        key: "adaptive_pauses",
        label: "adaptive pauses",
        schema: Schema::Daemon,
        kind: FieldKind::Bool,
    },
    Row {
        key: "discovery_cooldown_secs",
        label: "discovery cooldown",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::PositiveUint),
    },
    Row {
        key: "discovery_cooldown_cap_secs",
        label: "discovery cooldown cap",
        schema: Schema::Daemon,
        kind: FieldKind::Number(Number::PositiveUint),
    },
];

static GIT_SECTION: &[Row] = &[Row {
    key: "auto_push_remote",
    label: "auto-push remote",
    schema: Schema::Daemon,
    kind: FieldKind::Text,
}];

static DISPLAY_SECTION: &[Row] = &[
    Row {
        key: "theme",
        label: "theme",
        schema: Schema::Tui,
        kind: FieldKind::Enum(THEME_KEYS),
    },
    Row {
        key: "truecolor",
        label: "truecolor",
        schema: Schema::Tui,
        kind: FieldKind::Enum(&["auto", "on", "off"]),
    },
    Row {
        key: "preview_wrap",
        label: "preview wrap",
        schema: Schema::Tui,
        kind: FieldKind::Bool,
    },
    Row {
        key: "update_channel",
        label: "update channel",
        schema: Schema::Tui,
        kind: FieldKind::Enum(&["stable", "dev"]),
    },
    Row {
        key: "rail_mode",
        label: "rail mode",
        schema: Schema::Tui,
        kind: FieldKind::Enum(&["compact", "normal", "detailed"]),
    },
];

/// The overlay's sections in display order.
pub(crate) static SECTIONS: &[Section] = &[
    Section {
        title: "Provider and model",
        expanded: true,
        group: "provider_and_model",
        rows: PROVIDER_SECTION,
    },
    Section {
        title: "Pipeline",
        expanded: true,
        group: "pipeline",
        rows: PIPELINE_SECTION,
    },
    Section {
        title: "Timeouts and pauses",
        expanded: true,
        group: "timeouts_and_pauses",
        rows: TIMEOUTS_SECTION,
    },
    Section {
        title: "Git",
        expanded: true,
        group: "git",
        rows: GIT_SECTION,
    },
    Section {
        title: "Display and theme",
        expanded: true,
        group: "display_and_theme",
        rows: DISPLAY_SECTION,
    },
];

/// One focusable position of a collapsible group list: a group header or one
/// of its rows. The settings overlay uses `Entry<&'static Row>` over its
/// sections; the theme picker (T116.1) uses `Entry<&'static str>` over its
/// Dark/Light theme groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry<T> {
    Header(usize),
    Row(T),
}

impl<T> Entry<T> {
    /// The same entry with its row mapped through `f`; headers pass through.
    pub fn map_row<U, F: FnOnce(T) -> U>(self, f: F) -> Entry<U> {
        match self {
            Entry::Header(index) => Entry::Header(index),
            Entry::Row(row) => Entry::Row(f(row)),
        }
    }
}

/// The visible entries of a collapsible group list: every group's header
/// always, its rows (by reference) only while the group is expanded, in
/// group order -- the one fold mechanism the settings overlay's sections and
/// the theme picker's Dark/Light groups (T116.1) share.
pub fn group_entries<'a, T>(groups: &'a [&'a [T]], expanded: &[bool]) -> Vec<Entry<&'a T>> {
    let mut entries = Vec::new();
    for (index, rows) in groups.iter().enumerate() {
        entries.push(Entry::Header(index));
        if expanded.get(index).copied().unwrap_or(false) {
            entries.extend(rows.iter().map(Entry::Row));
        }
    }
    entries
}

/// Whether `code` acts on a group header and to which state it asks the group
/// to fold: Enter and Space toggle, Left folds an expanded header, Right
/// unfolds a folded one; every other key -- and the guarded reverse
/// directions -- is `None`, a no-op. The settings overlay's header keys and
/// the theme picker's (T116.1) share this one rule.
pub fn header_fold(code: KeyCode, expanded: bool) -> Option<bool> {
    match code {
        KeyCode::Enter | KeyCode::Char(' ') => Some(!expanded),
        KeyCode::Left if expanded => Some(false),
        KeyCode::Right if !expanded => Some(true),
        _ => None,
    }
}

/// Focus clamped into a visible entry list. Headers always render, so a group
/// list is never empty and the clamp always lands on a real entry.
pub fn clamp_focus(focus: usize, len: usize) -> usize {
    focus.min(len.saturating_sub(1))
}

/// The sections' row groups in display order, the input of the shared fold
/// mechanism ([`group_entries`]); parallel to [`SECTIONS`].
static SECTION_ROWS: [&[Row]; 5] = [
    PROVIDER_SECTION,
    PIPELINE_SECTION,
    TIMEOUTS_SECTION,
    GIT_SECTION,
    DISPLAY_SECTION,
];

/// The unsaved-changes dialog's choices (Esc or q on a dirty overlay): what the
/// three rows do, in the order they render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsChoice {
    /// Apply and persist every drafted change, then close the overlay.
    Save,
    /// Throw away every drafted change and close the overlay.
    Discard,
    /// Close the dialog; the overlay keeps its drafts.
    Cancel,
}

impl SettingsChoice {
    pub fn label(self) -> &'static str {
        match self {
            SettingsChoice::Save => "Save",
            SettingsChoice::Discard => "Discard",
            SettingsChoice::Cancel => "Cancel",
        }
    }

    /// The one-line explanation rendered after the label.
    pub fn detail(self) -> &'static str {
        match self {
            SettingsChoice::Save => "apply every drafted change and close",
            SettingsChoice::Discard => "throw away the drafted changes and close",
            SettingsChoice::Cancel => "back to the settings",
        }
    }
}

/// The dialog's rows, top to bottom; `confirm_selected` indexes this list.
pub const SETTINGS_CHOICES: [SettingsChoice; 3] = [
    SettingsChoice::Save,
    SettingsChoice::Discard,
    SettingsChoice::Cancel,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Error,
}

/// An open inline editor on one field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    pub field: &'static str,
    pub kind: FieldKind,
    pub buffer: String,
}

/// The overlay body scrolls to follow the cursor only once it comes this many rows
/// from an edge of the viewport, and then keeps that distance.
pub const SCROLL_MARGIN: usize = 4;

/// The scroll offset that keeps `cursor` (an index into the `len` visible rows)
/// inside a `height`-row viewport, moving `current` as little as possible: nothing
/// moves while the cursor sits at least [`SCROLL_MARGIN`] rows from either edge;
/// scrolling down starts only once the cursor would come closer than the margin to
/// the bottom edge, scrolling up only once it would come closer than the margin to
/// the top edge, and the offset then follows keeping exactly that margin. The
/// result always lies in `0..=len - height` with the cursor visible.
pub fn scroll_offset(cursor: usize, len: usize, height: usize, current: usize) -> usize {
    if len == 0 || height == 0 {
        return 0;
    }
    let cursor = cursor.min(len - 1);
    let max = len.saturating_sub(height);
    // A viewport too short to honour the margin at both edges shrinks it, so the
    // band below never inverts.
    let margin = SCROLL_MARGIN.min(height.saturating_sub(1) / 2);
    // Offsets keeping the cursor at least `margin` rows below the top edge ...
    let upper = cursor.saturating_sub(margin).min(max);
    // ... and at least `margin` rows above the bottom edge.
    let lower = (cursor + margin + 1).saturating_sub(height).min(max);
    current.clamp(lower, upper)
}

/// The overlay's interaction state; values live in [`crate::app::App`].
#[derive(Default)]
pub struct SettingsOverlay {
    pub open: bool,
    /// Per-section expanded flag, parallel to [`SECTIONS`].
    expanded: Vec<bool>,
    /// Index into the visible-row list.
    pub focus: usize,
    pub editor: Option<Editor>,
    /// The drafted, not yet applied changes, keyed by field: editing a value only
    /// updates this map; the save choice in the close dialog applies and persists
    /// every entry through the settings-change flow.
    pub drafts: BTreeMap<String, SettingValue>,
    /// The unsaved-changes dialog is open (Esc or q on a dirty overlay).
    pub confirm_open: bool,
    /// The dialog's selected row; indexes [`SETTINGS_CHOICES`].
    pub confirm_selected: usize,
    pub status: Option<(String, StatusLevel)>,
    /// The body's scroll offset, recomputed by the renderer through
    /// [`scroll_offset`] after every key press.
    pub scroll: Cell<usize>,
    /// The overlay's bottom line rect at the last render; the Close button's
    /// mouse hit-testing reads it (T59.1).
    pub footer: Cell<Rect>,
    /// The unsaved-changes dialog's bottom line rect at the last render; its
    /// buttons' mouse hit-testing reads it (T59.1).
    pub confirm_footer: Cell<Rect>,
    /// The overlay's close button rect at the last render (T66.1); a click on
    /// it runs the overlay's Esc key.
    pub close: Cell<Rect>,
    /// The unsaved-changes dialog's close button rect at the last render
    /// (T66.1); a click on it returns to the overlay like its Esc key.
    pub confirm_close: Cell<Rect>,
}

impl SettingsOverlay {
    /// Opens the overlay with fresh per-open state.
    pub fn open(&mut self) {
        self.open = true;
        self.expanded = SECTIONS.iter().map(|section| section.expanded).collect();
        self.focus = 0;
        self.editor = None;
        self.drafts.clear();
        self.confirm_open = false;
        self.confirm_selected = 0;
        self.status = None;
        self.scroll.set(0);
    }

    /// Closes the overlay, whatever state it is in.
    pub fn close(&mut self) {
        self.open = false;
        self.editor = None;
        self.drafts.clear();
        self.confirm_open = false;
        self.confirm_selected = 0;
        self.status = None;
    }

    /// Whether drafted changes exist that no save has applied yet.
    pub fn dirty(&self) -> bool {
        !self.drafts.is_empty()
    }

    /// The drafted changes in row-model order, for the driver's save flow to apply
    /// and persist one by one.
    pub fn pending(&self) -> Vec<(Schema, String, SettingValue)> {
        SECTIONS
            .iter()
            .flat_map(|section| section.rows.iter())
            .filter_map(|row| {
                self.drafts
                    .get(row.key)
                    .map(|value| (row.schema, row.key.to_string(), value.clone()))
            })
            .collect()
    }

    /// The focusable entries in display order: each section header and its rows when
    /// the section is expanded.
    pub fn visible(&self) -> Vec<Entry<&'static Row>> {
        group_entries(&SECTION_ROWS, &self.expanded)
    }

    pub(crate) fn expanded(&self, index: usize) -> bool {
        self.expanded.get(index).copied().unwrap_or(false)
    }

    /// The focused entry. The visible list is never empty -- every section always
    /// renders its header -- so this never fails.
    pub fn focused(&self) -> Entry<&'static Row> {
        let entries = self.visible();
        entries
            .get(self.focus.min(entries.len() - 1))
            .copied()
            .expect("the visible list is never empty")
    }

    /// The focused editable row, if any.
    pub fn focused_row(&self) -> Option<&'static Row> {
        match self.focused() {
            Entry::Row(row) => Some(row),
            _ => None,
        }
    }

    /// The help box's content for the focused entry (T85.1): the focused row's
    /// label and description, or the section's title and its group description
    /// on a header. The description comes from the owning schema's registry.
    pub fn focused_help(&self) -> Option<(&'static str, &'static str)> {
        match self.focused() {
            Entry::Row(row) => help_for(row.schema, row.key).map(|text| (row.label, text)),
            Entry::Header(index) => {
                let section = SECTIONS.get(index)?;
                // Sections are schema-homogeneous, so the first row's schema
                // is the section's.
                help_for(section.rows[0].schema, section.group).map(|text| (section.title, text))
            }
        }
    }

    fn toggle_section(&mut self, index: usize) {
        if let Some(flag) = self.expanded.get_mut(index) {
            *flag = !*flag;
        }
        // Headers always render, so folding the focused header never hides the focus:
        // focus only needs clamping into the (possibly shortened) visible list.
        let len = self.visible().len();
        self.focus = clamp_focus(self.focus, len);
    }

    fn move_focus(&mut self, delta: isize) {
        let len = self.visible().len();
        self.focus = (self.focus.saturating_add_signed(delta)).min(len - 1);
    }

    /// Handles one key. `current` is the focused row's current value -- the draft
    /// when one exists, otherwise the engine-reported readout or the shell's tui
    /// mirror; edits only update [`Self::drafts`], and the close dialog's save
    /// choice is what asks the driver to apply and persist them.
    pub fn on_key(&mut self, key: KeyEvent, current: Option<SettingValue>) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // The unsaved-changes dialog swallows every key while it is open.
        if self.confirm_open {
            return self.on_confirm_key(key.code);
        }
        if self.editor.is_some() {
            return self.on_editor_key(key, ctrl);
        }
        // hjkl alias the arrows everywhere the arrows act on the overlay (T25.1):
        // this is never reached while an editor is open (the letters keep typing
        // there) and the close dialog above treats j/k like the arrows too.
        let code = match key.code {
            KeyCode::Char('h') if !ctrl => KeyCode::Left,
            KeyCode::Char('j') if !ctrl => KeyCode::Down,
            KeyCode::Char('k') if !ctrl => KeyCode::Up,
            KeyCode::Char('l') if !ctrl => KeyCode::Right,
            code => code,
        };
        match code {
            KeyCode::Up => {
                self.move_focus(-1);
                Action::None
            }
            KeyCode::Down => {
                self.move_focus(1);
                Action::None
            }
            KeyCode::PageUp => {
                self.move_focus(-10);
                Action::None
            }
            KeyCode::PageDown => {
                self.move_focus(10);
                Action::None
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                self.on_row_key(code, current)
            }
            // q and Esc share the same close path: direct on a clean overlay,
            // the unsaved-changes dialog on a dirty one.
            KeyCode::Esc | KeyCode::Char('q') if !ctrl => self.begin_close(),
            _ => Action::None,
        }
    }

    /// The unsaved-changes dialog's keys: Up and k move the selection up,
    /// Down and j move it down, Enter confirms it, Esc cancels back to the
    /// overlay, and everything else is swallowed. Ctrl+C is handled before this
    /// runs (it detaches from anywhere).
    fn on_confirm_key(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.confirm_selected = self.confirm_selected.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.confirm_selected = (self.confirm_selected + 1).min(SETTINGS_CHOICES.len() - 1);
            }
            KeyCode::Esc => self.confirm_open = false,
            KeyCode::Enter => {
                self.confirm_open = false;
                return match SETTINGS_CHOICES[self.confirm_selected.min(SETTINGS_CHOICES.len() - 1)]
                {
                    // Save: the driver applies and persists every drafted change
                    // through the settings-change flow, then closes the overlay.
                    SettingsChoice::Save => Action::SaveSettings,
                    SettingsChoice::Discard => {
                        self.drafts.clear();
                        self.close();
                        Action::CloseSettings
                    }
                    SettingsChoice::Cancel => Action::None,
                };
            }
            _ => {}
        }
        Action::None
    }

    fn on_editor_key(&mut self, key: KeyEvent, ctrl: bool) -> Action {
        match key.code {
            // Esc closes the editor first; the buffer is discarded, not drafted.
            KeyCode::Esc => {
                self.editor = None;
                self.status = None;
                Action::None
            }
            KeyCode::Enter => {
                let Some(editor) = self.editor.take() else {
                    return Action::None;
                };
                match Self::parse_editor_buffer(&editor) {
                    // A valid buffer commits as a draft: no change is sent, applied
                    // or persisted until the close dialog's save choice.
                    Ok(value) => {
                        self.drafts.insert(editor.field.to_string(), value);
                        self.status = None;
                        Action::None
                    }
                    Err(error) => {
                        self.editor = Some(editor);
                        self.status = Some((error, StatusLevel::Error));
                        Action::None
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.buffer.pop();
                }
                Action::None
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.buffer.clear();
                }
                Action::None
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(editor) = self.editor.as_mut()
                    && Self::char_allowed(&editor.kind, c)
                {
                    editor.buffer.push(c);
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    fn on_row_key(&mut self, code: KeyCode, current: Option<SettingValue>) -> Action {
        match self.focused() {
            Entry::Header(index) => {
                // Enter/Space toggle, Left folds an expanded section, Right
                // unfolds a folded one; the guards make the reverse
                // directions no-ops (the shared header key rule).
                if header_fold(code, self.expanded(index)).is_some() {
                    self.toggle_section(index);
                }
                Action::None
            }
            Entry::Row(row) => self.on_field_key(row, code, current),
        }
    }

    fn on_field_key(
        &mut self,
        row: &'static Row,
        code: KeyCode,
        current: Option<SettingValue>,
    ) -> Action {
        match row.kind {
            FieldKind::ReadOnly => Action::None,
            FieldKind::Bool => {
                if matches!(code, KeyCode::Enter | KeyCode::Char(' ')) {
                    self.drafts.insert(
                        row.key.to_string(),
                        SettingValue::Bool(
                            !current.and_then(|value| value.as_bool()).unwrap_or(false),
                        ),
                    );
                }
                Action::None
            }
            FieldKind::Enum(values) => match code {
                KeyCode::Left => self.cycle_enum(row, values, current, -1),
                KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                    self.cycle_enum(row, values, current, 1)
                }
                _ => Action::None,
            },
            FieldKind::Number(_) | FieldKind::Text => {
                if code == KeyCode::Enter {
                    let prefill = current.map(|value| value.to_string()).unwrap_or_default();
                    self.editor = Some(Editor {
                        field: row.key,
                        kind: row.kind,
                        buffer: prefill,
                    });
                    self.status = Some((
                        format!(
                            "Editing {} -- Enter confirms, Ctrl+U clears, Esc cancels",
                            row.label
                        ),
                        StatusLevel::Info,
                    ));
                }
                Action::None
            }
        }
    }

    /// Cycles the enum one step (wrapping) and drafts the new choice; nothing is
    /// submitted until the close dialog's save choice.
    fn cycle_enum(
        &mut self,
        row: &'static Row,
        values: &'static [&'static str],
        current: Option<SettingValue>,
        step: isize,
    ) -> Action {
        let now = match &current {
            Some(SettingValue::Str(choice)) => choice.as_str(),
            _ => values[0],
        };
        let index = values.iter().position(|value| *value == now).unwrap_or(0);
        let next = values[((index as isize + step).rem_euclid(values.len() as isize)) as usize];
        self.drafts
            .insert(row.key.to_string(), enum_submit_value(row.key, next));
        Action::None
    }

    /// Esc/q: nothing drafted closes directly; a dirty overlay opens the unsaved-changes dialog asking what
    /// to do with the drafted changes instead.
    fn begin_close(&mut self) -> Action {
        if !self.dirty() {
            self.close();
            return Action::CloseSettings;
        }
        self.confirm_open = true;
        self.confirm_selected = 0;
        Action::None
    }

    /// What the inline editor accepts, per field kind.
    fn char_allowed(kind: &FieldKind, c: char) -> bool {
        match kind {
            FieldKind::Number(Number::Uint) | FieldKind::Number(Number::PositiveUint) => {
                c.is_ascii_digit()
            }
            FieldKind::Number(Number::Float) => c.is_ascii_digit() || c == '.' || c == '-',
            _ => !c.is_control(),
        }
    }

    /// Validates the inline editor's buffer locally (the engine and the shell re-check
    /// on apply); a violation keeps the editor open with the reason in the status line.
    fn parse_editor_buffer(editor: &Editor) -> Result<SettingValue, String> {
        match editor.kind {
            FieldKind::Number(Number::Uint) => {
                let secs: u32 = editor
                    .buffer
                    .parse()
                    .map_err(|_| format!("{} must be a whole number", editor.field))?;
                Ok(SettingValue::Uint(u64::from(secs)))
            }
            FieldKind::Number(Number::PositiveUint) => {
                let secs: u32 = editor
                    .buffer
                    .parse()
                    .map_err(|_| format!("{} must be a whole number", editor.field))?;
                if secs == 0 {
                    return Err(format!(
                        "{} must be a whole number of seconds greater than zero",
                        editor.field
                    ));
                }
                Ok(SettingValue::Uint(u64::from(secs)))
            }
            FieldKind::Number(Number::Float) => {
                let value: f64 = editor
                    .buffer
                    .parse()
                    .map_err(|_| format!("{} must be a number", editor.field))?;
                if !(0.0..=1.0).contains(&value) {
                    return Err(format!(
                        "{} must be a number between 0.0 and 1.0",
                        editor.field
                    ));
                }
                Ok(SettingValue::Float(value))
            }
            // Free text: the empty string clears an optional field on the apply side.
            _ => Ok(SettingValue::Str(editor.buffer.trim().to_string())),
        }
    }
}

/// The description of one entry of the row model (T85.1), routed to the registry
/// of the schema that owns the key: the daemon fields' and groups' text lives
/// with the daemon schema, the tui fields' with the tui schema -- both in
/// `patok-core`, where the settings live. The shell never hardcodes a
/// description itself.
fn help_for(schema: Schema, key: &str) -> Option<&'static str> {
    match schema {
        Schema::Daemon => daemon_help(key),
        Schema::Tui => tui_help(key),
    }
}

/// The value submitted for an enum choice. `truecolor` is the one enum stored as
/// `Option<bool>`: "auto" removes the key so the lower layers (and finally the
/// auto-detection) apply again, which needs [`SettingValue::Unset`].
fn enum_submit_value(field: &str, choice: &str) -> SettingValue {
    if field == "truecolor" {
        return match choice {
            "on" => SettingValue::Bool(true),
            "off" => SettingValue::Bool(false),
            _ => SettingValue::Unset,
        };
    }
    SettingValue::Str(choice.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay() -> SettingsOverlay {
        let mut overlay = SettingsOverlay::default();
        overlay.open();
        overlay
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn str(value: &str) -> Option<SettingValue> {
        Some(SettingValue::Str(value.into()))
    }

    /// Puts focus on the (visible) row of `field`.
    fn focus_field(overlay: &mut SettingsOverlay, field: &str) {
        let index = overlay
            .visible()
            .iter()
            .position(|entry| matches!(entry, Entry::Row(row) if row.key == field))
            .unwrap_or_else(|| panic!("`{field}` is not visible"));
        overlay.focus = index;
    }

    /// Puts focus on a section header.
    fn focus_header(overlay: &mut SettingsOverlay, index: usize) {
        let header = overlay
            .visible()
            .iter()
            .position(|entry| *entry == Entry::Header(index))
            .expect("the header is visible");
        overlay.focus = header;
    }

    /// Puts focus on a section header and toggles it.
    fn toggle_section(overlay: &mut SettingsOverlay, index: usize) {
        focus_header(overlay, index);
        overlay.on_key(key(KeyCode::Enter), None);
    }

    #[test]
    fn the_row_model_covers_the_documented_sections() {
        let subject = overlay();
        let entries = subject.visible();
        // Every section opens expanded: 5 headers plus all of their rows.
        assert_eq!(
            entries.len(),
            5 + PROVIDER_SECTION.len()
                + PIPELINE_SECTION.len()
                + TIMEOUTS_SECTION.len()
                + GIT_SECTION.len()
                + DISPLAY_SECTION.len()
        );
        assert_eq!(entries[0], Entry::Header(0));
        // The list ends on the last real entry row, with no button after it.
        assert_eq!(*entries.last().unwrap(), Entry::Row(&DISPLAY_SECTION[4]));
        assert!(entries.contains(&Entry::Row(&PROVIDER_SECTION[0])));
        assert!(entries.contains(&Entry::Row(&GIT_SECTION[0])));
        assert!(entries.contains(&Entry::Row(&DISPLAY_SECTION[0])));
        // A folded section contributes only its header.
        let mut overlay = overlay();
        overlay.toggle_section(4);
        assert!(
            overlay
                .visible()
                .iter()
                .all(|entry| !matches!(entry, Entry::Row(row) if row.schema == Schema::Tui))
        );
    }

    #[test]
    fn every_row_and_group_resolves_through_its_owning_registry() {
        // Every section's group id and every row's key must resolve to
        // non-empty help text through the registry of the row's declared
        // schema (T85.1) -- nothing on the overlay is without a description.
        for section in SECTIONS {
            let group = help_for(section.rows[0].schema, section.group)
                .unwrap_or_else(|| panic!("`{}` has no group help", section.group));
            assert!(!group.is_empty());
            for row in section.rows {
                let help = help_for(row.schema, row.key)
                    .unwrap_or_else(|| panic!("`{}` has no help text", row.key));
                assert!(!help.is_empty());
            }
        }
        // The overlay routes the focused entry through the same registries: a
        // fresh overlay focuses the first section's header, and a focused row
        // carries its own label and description.
        let mut subject = overlay();
        assert_eq!(
            subject.focused_help().map(|(label, _)| label),
            Some("Provider and model")
        );
        assert!(
            subject
                .focused_help()
                .is_some_and(|(_, text)| text.starts_with("Which CLI provider"))
        );
        focus_field(&mut subject, "run_mode");
        assert_eq!(
            subject.focused_help(),
            Some((
                "run mode",
                "sprint (default) or continuous. The running loop keeps the mode it started with; a change applies at the next run."
            ))
        );
    }

    #[test]
    fn collapsing_and_focus_stay_consistent() {
        let mut overlay = overlay();
        let full = overlay.visible().len();
        assert_eq!(overlay.focused(), Entry::Header(0));
        toggle_section(&mut overlay, 1);
        let entries = overlay.visible();
        assert_eq!(entries.len(), full - PIPELINE_SECTION.len());
        // Focus clamps into range and never lands on a hidden row.
        overlay.focus = usize::MAX / 2;
        assert!(overlay.focus >= entries.len());
        assert_eq!(overlay.focused(), Entry::Row(&DISPLAY_SECTION[4]));
        assert!(!entries.contains(&Entry::Row(&PIPELINE_SECTION[0])));
        // Expanding brings the rows back.
        toggle_section(&mut overlay, 1);
        assert_eq!(overlay.visible().len(), full);
    }

    #[test]
    fn left_folds_a_header_and_right_unfolds_it() {
        let mut overlay = overlay();
        focus_header(&mut overlay, 1);
        let full = overlay.visible().len();
        assert_eq!(overlay.on_key(key(KeyCode::Left), None), Action::None);
        assert!(!overlay.expanded(1));
        assert_eq!(overlay.visible().len(), full - PIPELINE_SECTION.len());
        // Folding the focused header keeps focus on it (the header never hides).
        assert_eq!(overlay.focused(), Entry::Header(1));
        assert_eq!(overlay.on_key(key(KeyCode::Right), None), Action::None);
        assert!(overlay.expanded(1));
        assert_eq!(overlay.visible().len(), full);
    }

    #[test]
    fn left_on_a_folded_header_and_right_on_an_expanded_one_are_noops() {
        let mut overlay = overlay();
        focus_header(&mut overlay, 1);
        overlay.toggle_section(1);
        let folded = overlay.visible();
        assert_eq!(overlay.on_key(key(KeyCode::Left), None), Action::None);
        assert!(!overlay.expanded(1));
        assert_eq!(overlay.visible(), folded);
        focus_header(&mut overlay, 0);
        let expanded = overlay.visible();
        assert_eq!(overlay.on_key(key(KeyCode::Right), None), Action::None);
        assert!(overlay.expanded(0));
        assert_eq!(overlay.visible(), expanded);
    }

    #[test]
    fn hjkl_alias_the_arrows() {
        // j/k move the cursor exactly like Down/Up.
        let mut hjkl = overlay();
        let mut arrows = overlay();
        for _ in 0..5 {
            assert_eq!(
                hjkl.on_key(key(KeyCode::Char('j')), None),
                arrows.on_key(key(KeyCode::Down), None)
            );
            assert_eq!(hjkl.focus, arrows.focus);
        }
        for _ in 0..3 {
            assert_eq!(
                hjkl.on_key(key(KeyCode::Char('k')), None),
                arrows.on_key(key(KeyCode::Up), None)
            );
            assert_eq!(hjkl.focus, arrows.focus);
        }

        // h/l fold and unfold headers like Left/Right, with the same ignore rules.
        let mut subject = overlay();
        focus_header(&mut subject, 1);
        let full = subject.visible().len();
        assert_eq!(subject.on_key(key(KeyCode::Char('h')), None), Action::None);
        assert!(!subject.expanded(1));
        assert_eq!(subject.visible().len(), full - PIPELINE_SECTION.len());
        // h on a folded header is a no-op, like Left.
        let folded = subject.visible();
        assert_eq!(subject.on_key(key(KeyCode::Char('h')), None), Action::None);
        assert_eq!(subject.visible(), folded);
        assert_eq!(subject.on_key(key(KeyCode::Char('l')), None), Action::None);
        assert!(subject.expanded(1));
        assert_eq!(subject.visible().len(), full);
        // l on an expanded header is a no-op, like Right.
        let expanded = subject.visible();
        assert_eq!(subject.on_key(key(KeyCode::Char('l')), None), Action::None);
        assert_eq!(subject.visible(), expanded);

        // h/l cycle enum fields like Left/Right, drafting the new choice.
        focus_field(&mut subject, "run_mode");
        assert_eq!(
            subject.on_key(key(KeyCode::Char('l')), str("sprint")),
            Action::None
        );
        assert_eq!(
            subject.drafts.get("run_mode"),
            Some(&SettingValue::Str("continuous".into()))
        );
        assert_eq!(
            subject.on_key(key(KeyCode::Char('h')), str("continuous")),
            Action::None
        );
        assert_eq!(
            subject.drafts.get("run_mode"),
            Some(&SettingValue::Str("sprint".into()))
        );

        // Inside an open text editor the four letters keep typing as characters.
        focus_field(&mut subject, "model");
        subject.on_key(key(KeyCode::Enter), Some(SettingValue::Str(String::new())));
        let before = subject.expanded.clone();
        for c in ['h', 'j', 'k', 'l'] {
            assert_eq!(subject.on_key(key(KeyCode::Char(c)), None), Action::None);
        }
        assert_eq!(subject.editor.as_ref().unwrap().buffer, "hjkl");
        assert!(subject.editor.is_some());
        assert_eq!(subject.expanded, before);

        // Ctrl-modified hjkl are not aliased.
        let mut ctrl_overlay = overlay();
        let focus = ctrl_overlay.focus;
        ctrl_overlay.on_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            None,
        );
        assert_eq!(ctrl_overlay.focus, focus);
    }

    #[test]
    fn left_and_right_keep_going_to_the_open_editor() {
        let mut overlay = overlay();
        focus_field(&mut overlay, "agent_timeout_secs");
        overlay.on_key(key(KeyCode::Enter), Some(SettingValue::Uint(600)));
        assert!(overlay.editor.is_some());
        let before = overlay.expanded.clone();
        assert_eq!(overlay.on_key(key(KeyCode::Left), None), Action::None);
        assert_eq!(overlay.on_key(key(KeyCode::Right), None), Action::None);
        // The editor stays open with its buffer untouched, and nothing folded.
        assert!(overlay.editor.is_some());
        assert_eq!(overlay.editor.as_ref().unwrap().buffer, "600");
        assert_eq!(overlay.expanded, before);
    }

    #[test]
    fn enums_cycle_wrapping_and_truecolor_maps_to_unset() {
        let mut overlay = overlay();
        focus_field(&mut overlay, "run_mode");
        assert_eq!(
            overlay.on_key(key(KeyCode::Right), str("sprint")),
            Action::None
        );
        assert_eq!(
            overlay.drafts.get("run_mode"),
            Some(&SettingValue::Str("continuous".into()))
        );
        // Enter cycles forward too, Left backward, both wrapping; each step drafts
        // (the caller passes the drafted value as `current`, as the app does).
        assert_eq!(
            overlay.on_key(key(KeyCode::Enter), str("continuous")),
            Action::None
        );
        assert_eq!(
            overlay.drafts.get("run_mode"),
            Some(&SettingValue::Str("sprint".into()))
        );
        assert_eq!(
            overlay.on_key(key(KeyCode::Left), str("sprint")),
            Action::None
        );
        assert_eq!(
            overlay.drafts.get("run_mode"),
            Some(&SettingValue::Str("continuous".into()))
        );

        // truecolor: auto -> on -> off -> auto, drafted as Unset/Bool(true)/Bool(false).
        focus_field(&mut overlay, "truecolor");
        for (current, expected) in [
            (str("auto"), SettingValue::Bool(true)),
            (str("on"), SettingValue::Bool(false)),
            (str("off"), SettingValue::Unset),
            (str("auto"), SettingValue::Bool(true)),
        ] {
            assert_eq!(overlay.on_key(key(KeyCode::Enter), current), Action::None);
            assert_eq!(overlay.drafts.get("truecolor"), Some(&expected));
        }

        // rail_mode: compact -> normal -> detailed -> compact.
        focus_field(&mut overlay, "rail_mode");
        for (current, expected) in [
            (str("compact"), "normal"),
            (str("normal"), "detailed"),
            (str("detailed"), "compact"),
        ] {
            assert_eq!(overlay.on_key(key(KeyCode::Right), current), Action::None);
            assert_eq!(
                overlay.drafts.get("rail_mode"),
                Some(&SettingValue::Str(expected.into()))
            );
        }
    }

    #[test]
    fn the_editor_validates_before_submitting() {
        let mut overlay = overlay();
        focus_field(&mut overlay, "agent_timeout_secs");
        assert_eq!(
            overlay.on_key(key(KeyCode::Enter), Some(SettingValue::Uint(600))),
            Action::None
        );
        assert_eq!(overlay.editor.as_ref().unwrap().buffer, "600");
        // Digits type, letters do not, Ctrl+U clears.
        overlay.on_key(key(KeyCode::Char('2')), None);
        assert_eq!(overlay.editor.as_ref().unwrap().buffer, "6002");
        overlay.on_key(key(KeyCode::Char('x')), None);
        assert_eq!(overlay.editor.as_ref().unwrap().buffer, "6002");
        overlay.on_key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            None,
        );
        assert_eq!(overlay.editor.as_ref().unwrap().buffer, "");
        // Zero is a local violation: the editor stays open with the reason.
        overlay.on_key(key(KeyCode::Char('0')), None);
        assert_eq!(overlay.on_key(key(KeyCode::Enter), None), Action::None);
        assert!(overlay.editor.is_some());
        assert_eq!(overlay.status.as_ref().unwrap().1, StatusLevel::Error);
        assert!(
            overlay
                .status
                .as_ref()
                .unwrap()
                .0
                .contains("greater than zero")
        );
        // A valid value commits as a draft and closes the editor.
        overlay.on_key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            None,
        );
        for c in "120".chars() {
            overlay.on_key(key(KeyCode::Char(c)), None);
        }
        assert_eq!(overlay.on_key(key(KeyCode::Enter), None), Action::None);
        assert!(overlay.editor.is_none());
        assert_eq!(
            overlay.drafts.get("agent_timeout_secs"),
            Some(&SettingValue::Uint(120))
        );
        // Esc discards the edit instead.
        overlay.on_key(key(KeyCode::Enter), Some(SettingValue::Uint(120)));
        assert!(overlay.editor.is_some());
        assert_eq!(overlay.on_key(key(KeyCode::Esc), None), Action::None);
        assert!(overlay.editor.is_none());
    }

    #[test]
    fn the_close_dialog_saves_discards_or_cancels() {
        // Clean: closes directly.
        let mut subject = overlay();
        assert_eq!(
            subject.on_key(key(KeyCode::Esc), None),
            Action::CloseSettings
        );
        assert!(!subject.open);

        // Dirty: Esc opens the three-choice dialog instead of closing.
        let mut subject = overlay();
        subject
            .drafts
            .insert("plan_enabled".into(), SettingValue::Bool(true));
        assert!(subject.dirty());
        assert_eq!(subject.on_key(key(KeyCode::Esc), None), Action::None);
        assert!(subject.confirm_open);
        assert_eq!(subject.confirm_selected, 0);
        // Up clamps at the first row; Down walks to the last and clamps there.
        assert_eq!(subject.on_key(key(KeyCode::Up), None), Action::None);
        assert_eq!(subject.confirm_selected, 0);
        subject.on_key(key(KeyCode::Down), None);
        assert_eq!(subject.confirm_selected, 1);
        subject.on_key(key(KeyCode::Down), None);
        assert_eq!(subject.confirm_selected, 2);
        subject.on_key(key(KeyCode::Down), None);
        assert_eq!(subject.confirm_selected, 2);
        // Everything else is swallowed while the dialog is open.
        let before = (subject.confirm_selected, subject.drafts.clone());
        assert_eq!(subject.on_key(key(KeyCode::Char('x')), None), Action::None);
        assert_eq!((subject.confirm_selected, subject.drafts.clone()), before);
        // Esc cancels: back to the overlay with the drafts intact.
        assert_eq!(subject.on_key(key(KeyCode::Esc), None), Action::None);
        assert!(!subject.confirm_open);
        assert!(subject.dirty());

        // Save asks the driver to apply every draft; the overlay stays open until
        // the driver finishes.
        subject.on_key(key(KeyCode::Esc), None);
        assert_eq!(
            subject.on_key(key(KeyCode::Enter), None),
            Action::SaveSettings
        );
        assert!(!subject.confirm_open);
        assert!(subject.open);
        assert_eq!(
            subject.pending(),
            vec![(
                Schema::Daemon,
                "plan_enabled".to_string(),
                SettingValue::Bool(true)
            )]
        );

        // Discard throws the drafts away and closes.
        let mut subject = overlay();
        subject
            .drafts
            .insert("plan_enabled".into(), SettingValue::Bool(true));
        subject.on_key(key(KeyCode::Esc), None);
        subject.on_key(key(KeyCode::Down), None);
        assert_eq!(
            subject.on_key(key(KeyCode::Enter), None),
            Action::CloseSettings
        );
        assert!(!subject.open);
        assert!(subject.drafts.is_empty());

        // Cancel (the third row) leaves the overlay open with the drafts intact.
        let mut subject = overlay();
        subject
            .drafts
            .insert("plan_enabled".into(), SettingValue::Bool(true));
        subject.on_key(key(KeyCode::Esc), None);
        subject.on_key(key(KeyCode::Down), None);
        subject.on_key(key(KeyCode::Down), None);
        assert_eq!(subject.on_key(key(KeyCode::Enter), None), Action::None);
        assert!(subject.open);
        assert!(subject.dirty());
    }

    #[test]
    fn the_close_dialog_moves_with_j_and_k() {
        // Dirty: Esc opens the dialog with Save selected.
        let mut subject = overlay();
        subject
            .drafts
            .insert("plan_enabled".into(), SettingValue::Bool(true));
        assert!(subject.dirty());
        assert_eq!(subject.on_key(key(KeyCode::Esc), None), Action::None);
        assert!(subject.confirm_open);
        assert_eq!(subject.confirm_selected, 0);
        // k clamps at the first row; j walks to the last and clamps there.
        assert_eq!(subject.on_key(key(KeyCode::Char('k')), None), Action::None);
        assert_eq!(subject.confirm_selected, 0);
        subject.on_key(key(KeyCode::Char('j')), None);
        assert_eq!(subject.confirm_selected, 1);
        subject.on_key(key(KeyCode::Char('j')), None);
        assert_eq!(subject.confirm_selected, 2);
        subject.on_key(key(KeyCode::Char('j')), None);
        assert_eq!(subject.confirm_selected, 2);
        // Enter on Cancel (row 2) leaves the overlay open with drafts intact.
        assert_eq!(subject.on_key(key(KeyCode::Enter), None), Action::None);
        assert!(!subject.confirm_open);
        assert!(subject.open);
        assert!(subject.dirty());

        // Reopening resets to Save; a j move selects Discard, which closes and
        // throws the drafts away.
        subject.on_key(key(KeyCode::Esc), None);
        assert!(subject.confirm_open);
        assert_eq!(subject.confirm_selected, 0);
        subject.on_key(key(KeyCode::Char('j')), None);
        assert_eq!(
            subject.on_key(key(KeyCode::Enter), None),
            Action::CloseSettings
        );
        assert!(!subject.open);
        assert!(subject.drafts.is_empty());
    }

    #[test]
    fn down_scrolling_holds_then_advances_keeping_the_margin() {
        // len=40, height=20: the margin band keeps the cursor at viewport rows
        // 4..=15; the view stays put until the cursor would pass row 15.
        for cursor in 0..=15 {
            assert_eq!(scroll_offset(cursor, 40, 20, 0), 0, "cursor {cursor}");
        }
        // Chained Down presses: once scrolling starts the cursor keeps the 4-row
        // margin above the bottom edge (cursor - offset == 15), until the
        // list-end clamp stops the offset at max = 20 and the cursor rides the
        // last rows to the bottom edge.
        let mut current = 0;
        for cursor in 0..40 {
            current = scroll_offset(cursor, 40, 20, current);
            assert!(current <= 20);
            if (16..=35).contains(&cursor) {
                assert_eq!(cursor - current, 15, "cursor {cursor}");
            }
        }
        assert_eq!(current, 20);
    }

    #[test]
    fn up_scrolling_is_symmetric_with_the_same_margin() {
        // From the bottom (offset at max = 20) the view stays put while the cursor
        // climbs to viewport row 4 -- 4 rows below the top edge.
        for cursor in (24..=39).rev() {
            assert_eq!(scroll_offset(cursor, 40, 20, 20), 20, "cursor {cursor}");
        }
        // Past that point the offset decreases one row per Up press, keeping
        // the cursor at viewport row 4.
        for (cursor, expected) in [(23, 19), (22, 18), (21, 17), (20, 16)] {
            assert_eq!(
                scroll_offset(cursor, 40, 20, 20),
                expected,
                "cursor {cursor}"
            );
            assert_eq!(cursor - expected, 4);
        }
        let mut current = 20;
        for cursor in (0..40).rev() {
            current = scroll_offset(cursor, 40, 20, current);
            assert!(current <= cursor && cursor - current <= 19);
        }
        assert_eq!(current, 0);
    }

    #[test]
    fn the_offset_never_hides_the_cursor() {
        // A stale offset self-heals: any result keeps the cursor inside the
        // viewport and within the list's scrollable range.
        for (cursor, len, height, current) in [
            (39, 40, 20, 0),
            (0, 40, 20, 39),
            (15, 33, 28, 0),
            (32, 33, 28, 0),
            (22, 23, 20, 0),
        ] {
            let offset = scroll_offset(cursor, len, height, current);
            assert!(offset <= len.saturating_sub(height), "cursor {cursor}");
            assert!(offset <= cursor, "cursor {cursor}");
            assert!(cursor - offset < height, "cursor {cursor}, offset {offset}");
        }
    }

    #[test]
    fn degenerate_inputs_are_tolerated() {
        assert_eq!(scroll_offset(0, 0, 20, 7), 0);
        assert_eq!(scroll_offset(0, 15, 20, 7), 0);
        assert_eq!(scroll_offset(5, 15, 20, 7), 0);
        assert_eq!(scroll_offset(0, 40, 0, 7), 0);
        // A cursor past the end clamps to the last row.
        assert_eq!(scroll_offset(usize::MAX / 2, 40, 20, 35), 20);
    }

    #[test]
    fn a_short_viewport_shrinks_the_margin_without_panicking() {
        // height=6 shrinks the margin to 2 (band [2, 3]); height=2 and height=1
        // drop it to 0, leaving the smallest offset that still shows the cursor.
        for (height, expected) in [(6, 2), (2, 4), (1, 5)] {
            assert_eq!(scroll_offset(5, 40, height, 0), expected, "height {height}");
        }
        for height in 1..=12 {
            for cursor in 0..40 {
                let offset = scroll_offset(cursor, 40, height, 0);
                assert!(offset <= cursor && cursor - offset < height);
            }
        }
    }
}
