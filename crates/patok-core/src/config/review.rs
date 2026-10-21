//! The review finding severities: the one shared spelling of the reviewer
//! report's JSON buckets, the verdict computation and the multipass merge.

use crate::settings::config_enum;

config_enum! {
    /// One review finding's severity: the buckets of the reviewer report's
    /// findings JSON block, the verdict computation and the multipass merge
    /// all read the same three spellings -- `high`, `medium` and `low` --
    /// from here. The JSON block's field names are the file format and stay
    /// strings; unknown severity keys in a parsed block are dropped as before.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
    pub enum Severity {
        High => "high",
        Medium => "medium",
        Low => "low",
    }
}
