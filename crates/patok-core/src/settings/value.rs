//! The settings-change value and its apply timing, shared by the config
//! persistence, the engine events and the output.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A settings value as submitted by a settings change, mapped onto the field's expected
/// type by the per-schema field registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SettingValue {
    Bool(bool),
    Uint(u64),
    Float(f64),
    Str(String),
    /// Remove the key from its resolved layer file, so the lower layers (and finally
    /// the default) apply again. The only field that needs it is `tui.truecolor`, whose
    /// "auto" state is the absence of the key; the engine never reports it.
    Unset,
}

impl SettingValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::Uint(u) => Some(*u),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

impl fmt::Display for SettingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(b) => write!(f, "{b}"),
            Self::Uint(u) => write!(f, "{u}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Str(s) => write!(f, "{s}"),
            Self::Unset => write!(f, "(not set)"),
        }
    }
}

/// When a settings change takes effect: a field
/// that parameterizes a unit of work (a build-loop start, a task, an agent session) keeps
/// the value that unit started with and takes effect at the next such start; a field read
/// at the point of use applies immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyTiming {
    Immediate,
    NextUnitOfWork,
}
