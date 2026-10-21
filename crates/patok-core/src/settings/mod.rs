//! The config primitives patok-core itself uses: the enum-spelling trait and
//! macros, and the settings-change value with its apply timing. `crate::config`
//! re-exports them for its existing consumers.

mod spelling;
mod value;

pub use spelling::EnumSpelling;
// Reachable crate-wide, so the config enums and the enums outside the config
// tree (the agent role, the pipeline stage, the update channel) share one macro.
pub(crate) use spelling::{config_enum, config_enum_aliased};
pub use value::{ApplyTiming, SettingValue};
