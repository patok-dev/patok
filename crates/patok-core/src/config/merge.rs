//! The deep merge of the file layers, applied independently
//! per schema: a higher-precedence value at a given key overrides the lower one, keys a
//! file omits keep the lower layer's value. The daemon schema's layer merge is generated
//! with the rest of its field tables in `daemon.rs` (including the deeper merge of the
//! stage list by stage id); this module holds the
//! tui schema's merge.

use super::tui::TuiLayer;

/// Merges two tui layers, `higher` winning per key.
pub(crate) fn tui(lower: TuiLayer, higher: TuiLayer) -> TuiLayer {
    TuiLayer {
        theme: higher.theme.or(lower.theme),
        truecolor: higher.truecolor.or(lower.truecolor),
        preview_wrap: higher.preview_wrap.or(lower.preview_wrap),
        agent_pane_split: higher.agent_pane_split.or(lower.agent_pane_split),
        update_channel: higher.update_channel.or(lower.update_channel),
        rail_mode: higher.rail_mode.or(lower.rail_mode),
    }
}
