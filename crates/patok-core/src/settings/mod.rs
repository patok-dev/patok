//! The config primitives patok-core itself uses: the enum-spelling trait and
//! macros, and the settings-change value with its apply timing. The
//! `patok-config` crate re-exports them for its consumers.

mod spelling;
mod value;

pub use spelling::EnumSpelling;
// Public, so `patok-config` defines its enums with the same macro as the enums
// patok-core owns (the agent role, the pipeline stage, the update channel).
// `#[macro_export]` also places them at the crate root.
pub use crate::{config_enum, config_enum_aliased};
pub use value::{ApplyTiming, SettingValue};
