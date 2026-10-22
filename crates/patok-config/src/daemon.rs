//! The daemon schema: everything that parameterizes the
//! build loop, owned by the engine. The shell never reads this schema; it parses only the
//! `[daemon]` table of each config file.
//!
//! Every setting is declared once, in the `daemon_table!` invocation below: one entry
//! carries the key, type, default, overlay label, help text and validation, and the
//! macro derives every
//! table that used to be hand-listed beside it -- the settings struct with its defaults,
//! the raw option layer, the per-key parser, the layer merge, the materialization, the
//! persistence registry (field kind, key list, apply timing, effective values) and the
//! settings-overlay label and help. Adding a setting is one entry.

use std::path::Path;

use schemars::JsonSchema;

use patok_providers::ProviderName;

use super::{ConfigFiles, Fields, load_layers, migrate_project_file, project_file};
use patok_core::settings::{EnumSpelling, SettingValue, config_enum};

use patok_core::pipeline::Stage;

config_enum! {
    /// The run mode.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
    #[schemars(rename_all = "lowercase")]
    pub enum RunMode {
        #[default]
        Sprint => "sprint",
        Continuous => "continuous",
    }
}

config_enum! {
    /// The plan self-review accept policy.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
    #[schemars(rename_all = "kebab-case")]
    pub enum AcceptPolicy {
        #[default]
        NoHigh => "no-high",
        NoHighMedium => "no-high-medium",
        NoFindings => "no-findings",
    }
}

/// One pipeline stage: id, label, enabled flag and an optional
/// prompt override. A stage absent from the list is treated as disabled by the consumer;
/// order is display-only.
#[derive(Clone, Debug, PartialEq, Eq, JsonSchema)]
#[schemars(rename = "Stage")]
pub struct StageConfig {
    #[schemars(rename = "id", schema_with = "stage_id_schema")]
    pub stage: Stage,
    pub label: String,
    pub enabled: bool,
    pub prompt: Option<String>,
}

/// The schema of the `id` property: the accepted stage-id spellings as the
/// [`StageId`] enum's schema, so the committed schema and the parser derive from the
/// same list and cannot drift.
fn stage_id_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    generator.subschema_for::<StageId>()
}

/// The accepted stage ids, as the unknown-id warning names them.
const ACCEPTED_STAGE_IDS: &str = "research, plan, build (alias implement), review";

config_enum! {
    /// A stage id as written in a `[[daemon.stages]]` entry: the four pipeline
    /// stage spellings plus `implement`, the build stage's pre-enum spelling.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
    #[schemars(rename_all = "lowercase")]
    enum StageId {
        Research => "research",
        Plan => "plan",
        Build => "build",
        Implement => "implement",
        Review => "review",
    }
}

impl StageId {
    /// The pipeline stage the id keys: `implement` is the build stage's
    /// pre-enum spelling, kept so existing config files keep loading.
    fn stage(self) -> Stage {
        match self {
            Self::Research => Stage::Research,
            Self::Plan => Stage::Plan,
            Self::Build | Self::Implement => Stage::Build,
            Self::Review => Stage::Review,
        }
    }
}

/// Parses a stage id: one of the accepted spellings, trimmed, case-insensitive;
/// `None` for anything unrecognized.
fn parse_stage_id(id: &str) -> Option<Stage> {
    StageId::parse(id).map(StageId::stage)
}

/// A pipeline stage as written in one layer, before merging: each field may be absent,
/// and a higher layer's entry overrides only the fields it sets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RawStage {
    pub stage: Stage,
    pub label: Option<String>,
    pub enabled: Option<bool>,
    pub prompt: Option<String>,
}

/// What a field accepts; the persistence validator maps a submitted value onto the
/// field's expected type through it. Lives beside the field table, whose entries pick
/// each field's kind -- including the `FloatRange` bounds, read by both the on-disk
/// parser and the write-path validator so the two cannot drift apart again.
///
/// The cross-crate drift surface of finding 10: the settings overlay's row model and
/// both generated proto mappers (`patok-wire`'s `to_proto` and `from_proto`) read
/// the kind through [`daemon_field`], and the drift test in `patok-tui` fails the
/// build when a row's editor disagrees with the registry.
pub enum FieldKind {
    Bool,
    Uint,
    /// An unsigned integer that must be greater than zero, validated exactly like the
    /// on-disk parser of the same field ("a whole number of seconds greater than zero").
    PositiveUint,
    /// A float field with an inclusive range.
    FloatRange(f64, f64),
    /// A non-optional string; an empty string is rejected.
    Str,
    /// An optional string; the empty string removes the key so the default applies.
    OptStr,
    /// A string with a default whose empty value is meaningful (it switches the
    /// feature off), so the empty string is stored as written.
    FreeStr,
    /// An enumerated field and its accepted spellings (trimmed, lowercased).
    Enum(&'static [&'static str]),
    /// A provider field, accepted by `ProviderName::parse` (trimmed,
    /// lowercased, aliases accepted).
    Provider,
    /// An unsigned integer with an inclusive range.
    Range(u64, u64),
    /// A known key of the other (composite) shape that the settings-change flow cannot
    /// write as a single value.
    Composite,
}

// The one place a daemon setting is declared (finding 10 of the structure review): the
// `daemon_table!` invocation below lists every `[daemon]` field once, and the macros
// here derive from it every table that used to be hand-maintained beside it -- the
// settings struct with its `Default`, the raw `DaemonLayer`, the per-key parser
// `parse_layer`, the layer `merge`, the `materialize`d defaults plus the trailing
// normalization block, and the persistence registry -- `daemon_field` (what a key
// accepts), `DAEMON_FIELDS` (the single-value keys, in table order),
// `IMMEDIATE_DAEMON_FIELDS` (the `immediate`-marked keys), `daemon_field_value` (the
// effective value of each key) and `field_help` (the per-key overlay help). Every
// consumer reads the same entry, so the tables cannot disagree.
//
// Entry grammar, one bracketed item per field:
// `[kind field "key" (label "text")? (help "text")? (immediate)? <kind tail>]` with `bool`,
// `str` and `free_str` tail `default <expr>`, `uint` and `positive_uint` tail
// `expect "<warning noun>" default <expr>`, `float_range` tail
// `range <min> ..= <max> default <expr>`, `enum` its enum type right after the key,
// and no tail for
// `opt_str`, `provider` and `stages`. A leading `//` documents
// the settings-struct field (and so the JSON schema); `label` adds the settings-overlay
// row label; `help` adds the settings-overlay help text (absent means the overlay
// shows no label or help for the key); `immediate` marks a field read at the point of
// use.
//
// The derivation is split by expansion position (macro_rules cannot expand into struct
// fields or match arms): the two structs, the merged layer literal, the materialized
// settings literal and the two key lists are built by the recursive `daemon_settings!`,
// `daemon_layer!`, `daemon_merge!`, `daemon_materialize!`, `daemon_names!` and
// `daemon_immediate!` munchers, which accumulate one entry's fragment per step and
// emit the whole item at the end; the per-key runtime behaviour (parse, registry
// lookup, effective value, overlay help) comes from `daemon_entry!`, invoked once per
// entry at statement position.

/// The settings-struct field type of one table entry: the single per-kind type both
/// struct munchers read.
macro_rules! daemon_ty {
    (bool $($rest:tt)*) => (bool);
    (uint $($rest:tt)*) => (u64);
    (positive_uint $($rest:tt)*) => (u64);
    (float_range $($rest:tt)*) => (f64);
    (str $($rest:tt)*) => (String);
    (free_str $($rest:tt)*) => (String);
    (opt_str $($rest:tt)*) => (Option<String>);
    (enum $ty:ident $($rest:tt)*) => ($ty);
    (provider $($rest:tt)*) => (ProviderName);
    (stages $($rest:tt)*) => (Vec<StageConfig>);
}

/// The settings struct, accumulated one table entry at a time: each step appends the
/// entry's documented field (its type through [`daemon_ty!`]) and recurses; the base
/// arm emits the whole struct.
macro_rules! daemon_settings {
    (docs [$($sdoc:tt)*] fields [$($acc:tt)*]) => {
        $($sdoc)*
        #[derive(Clone, Debug, PartialEq, JsonSchema)]
        pub struct DaemonSettings {
            $($acc)*
        }
    };
    (
        docs [$($sdoc:tt)*] fields [$($acc:tt)*]
        [$(#[$doc:meta])* $kind:ident $f:ident $k:literal $($rest:tt)*]
        $($tail:tt)*
    ) => {
        daemon_settings! {
            docs [$($sdoc)*]
            fields [$($acc)* $(#[$doc])* pub $f: daemon_ty!($kind $($rest)*),]
            $($tail)*
        }
    };
}

/// The raw-layer field type of one table entry: every kind is the settings type
/// wrapped in `Option`, except `stages` (its layer holds [`RawStage`]) and `opt_str`
/// (its layer holds the same `Option<String>` -- an empty string already parses to
/// `None`).
macro_rules! daemon_layer_ty {
    (stages $($rest:tt)*) => (Option<Vec<RawStage>>);
    (opt_str $($rest:tt)*) => (Option<String>);
    ($($rest:tt)*) => (Option<daemon_ty!($($rest)*)>);
}

/// The raw option layer, accumulated one table entry at a time: each step appends the
/// entry's `Option` field and recurses; the base arm emits the whole struct.
macro_rules! daemon_layer {
    (fields [$($acc:tt)*]) => {
        /// One layer's `[daemon]` table; unset keys fall through to the layer below.
        #[derive(Clone, Debug, Default, PartialEq)]
        pub(crate) struct DaemonLayer {
            $($acc)*
        }
    };
    (
        fields [$($acc:tt)*]
        [$(#[$doc:meta])* $kind:ident $f:ident $k:literal $($rest:tt)*]
        $($tail:tt)*
    ) => {
        daemon_layer! {
            fields [$($acc)* pub $f: daemon_layer_ty!($kind $($rest)*),]
            $($tail)*
        }
    };
}

/// The merged layer literal: each step appends one entry's merge rule (plain values
/// higher-wins; the structured kinds merge deeper) and recurses; the base arm emits the
/// whole literal.
macro_rules! daemon_merge {
    (merged [$($acc:tt)*] $lower:ident, $higher:ident,) => {
        DaemonLayer {
            $($acc)*
        }
    };
    (
        merged [$($acc:tt)*] $lower:ident, $higher:ident,
        [$(#[$doc:meta])* stages $f:ident $k:literal $($rest:tt)*]
        $($tail:tt)*
    ) => {
        daemon_merge! {
            merged [$($acc)* $f: merge_stages($lower.$f, $higher.$f),]
            $lower, $higher, $($tail)*
        }
    };
    (
        merged [$($acc:tt)*] $lower:ident, $higher:ident,
        [$(#[$doc:meta])* $kind:ident $f:ident $k:literal $($rest:tt)*]
        $($tail:tt)*
    ) => {
        daemon_merge! {
            merged [$($acc)* $f: $higher.$f.or($lower.$f),]
            $lower, $higher, $($tail)*
        }
    };
}

/// The materialized settings literal: each step appends one entry's
/// default-or-override rule and recurses; the base arm emits the whole literal.
macro_rules! daemon_materialize {
    (settings [$($acc:tt)*] $layer:ident,) => {
        DaemonSettings {
            $($acc)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* bool $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? default $d:expr]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f.unwrap_or($d),]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* uint $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? expect $e:literal default $d:expr]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f.unwrap_or($d),]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* positive_uint $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? expect $e:literal default $d:expr]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f.unwrap_or($d),]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* float_range $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? range $min:literal ..= $max:literal default $d:expr]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f.unwrap_or($d),]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* str $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? default $d:expr]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f.unwrap_or_else(|| $d),]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* free_str $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? default $d:expr]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f.unwrap_or_else(|| $d),]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* opt_str $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)?]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f,]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* enum $f:ident $k:literal $ty:ident $(label $l:literal)? $(help $h:literal)? $(immediate)?]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f.unwrap_or_default(),]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* provider $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)?]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)* $f: $layer.$f.unwrap_or_default(),]
            $layer, $($tail)*
        }
    };
    (
        settings [$($acc:tt)*] $layer:ident,
        [$(#[$doc:meta])* stages $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)?]
        $($tail:tt)*
    ) => {
        daemon_materialize! {
            settings [$($acc)*
                $f: $layer
                    .$f
                    .map(|stages| {
                        stages
                            .into_iter()
                            .map(|stage| StageConfig {
                                label: stage
                                    .label
                                    .unwrap_or_else(|| stage.stage.as_str().into()),
                                enabled: stage.enabled.unwrap_or(true),
                                prompt: stage.prompt,
                                stage: stage.stage,
                            })
                            .collect()
                    })
                    .unwrap_or_else(default_stages),
            ]
            $layer, $($tail)*
        }
    };
}

/// The single-value key list: each step appends one entry's key (composite entries
/// contribute none) and recurses; the base arm emits the whole slice literal.
macro_rules! daemon_names {
    (keys [$($acc:tt)*]) => {
        &[
            $($acc)*
        ]
    };
    (
        keys [$($acc:tt)*]
        [$(#[$doc:meta])* stages $f:ident $k:literal $($rest:tt)*]
        $($tail:tt)*
    ) => {
        daemon_names! {
            keys [$($acc)*]
            $($tail)*
        }
    };
    (
        keys [$($acc:tt)*]
        [$(#[$doc:meta])* $kind:ident $f:ident $k:literal $($rest:tt)*]
        $($tail:tt)*
    ) => {
        daemon_names! {
            keys [$($acc)* $k,]
            $($tail)*
        }
    };
}

/// The immediate-key list: each step appends the key of one `immediate`-marked entry
/// (everything else contributes nothing) and recurses; the base arm emits the whole
/// slice literal.
macro_rules! daemon_immediate {
    (keys [$($acc:tt)*]) => {
        &[
            $($acc)*
        ]
    };
    (
        keys [$($acc:tt)*]
        [$(#[$doc:meta])* enum $f:ident $k:literal $ty:ident $(label $l:literal)? $(help $h:literal)? immediate $($rest:tt)*]
        $($tail:tt)*
    ) => {
        daemon_immediate! {
            keys [$($acc)* $k,]
            $($tail)*
        }
    };
    (
        keys [$($acc:tt)*]
        [$(#[$doc:meta])* $kind:ident $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? immediate $($rest:tt)*]
        $($tail:tt)*
    ) => {
        daemon_immediate! {
            keys [$($acc)* $k,]
            $($tail)*
        }
    };
    (
        keys [$($acc:tt)*]
        [$($skip:tt)*]
        $($tail:tt)*
    ) => {
        daemon_immediate! {
            keys [$($acc)*]
            $($tail)*
        }
    };
}

/// One table entry's per-key runtime behaviour, named by the first argument: the parse
/// statement (`parse`: one known-key check, ending the loop iteration so unknown keys
/// fall through to the warning), the registry lookup (`kind`), the effective value
/// (`value`), the overlay label (`label`) and the overlay help (`help`), each expanding
/// to one early-return
/// statement. The ident arguments name the runtime binding the expansion lands on
/// (`layer`, `f`, `key`, `value`, `errors`, `name`, `settings`, `field`), so hygiene
/// puts each inside the function that owns it.
macro_rules! daemon_entry {
    // The parse statement.
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* bool $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.boolean($key, $value);
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* uint $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? expect $e:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.uint($key, $value, $e, false);
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* positive_uint $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? expect $e:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.uint($key, $value, $e, true);
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* float_range $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? range $min:literal ..= $max:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.float($key, $value, Some(($min, $max)));
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* str $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.string($key, $value);
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* free_str $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.free_string($key, $value);
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* opt_str $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.string($key, $value);
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* enum $f:ident $k:literal $ty:ident $(label $l:literal)? $(help $h:literal)? $(immediate)?]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.enumerated($key, $value, &$ty::accepted_spellings(), $ty::parse);
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* provider $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.provider($key, $value, $errors);
            continue;
        }
    };
    (
        parse, $layer:ident, $fields:ident, $key:ident, $value:ident, $errors:ident,
        [$(#[$doc:meta])* stages $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $key.as_str() == $k {
            $layer.$f = $fields.stages($value);
            continue;
        }
    };
    // The registry lookup.
    (
        kind, $name:ident,
        [$(#[$doc:meta])* bool $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $name == $k {
            return Some(FieldKind::Bool);
        }
    };
    (kind, $name:ident, [$(#[$doc:meta])* uint $f:ident $k:literal $($rest:tt)*]) => {
        if $name == $k {
            return Some(FieldKind::Uint);
        }
    };
    (
        kind, $name:ident,
        [$(#[$doc:meta])* positive_uint $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $name == $k {
            return Some(FieldKind::PositiveUint);
        }
    };
    (
        kind, $name:ident,
        [$(#[$doc:meta])* float_range $f:ident $k:literal $(label $l:literal)? $(help $h:literal)? $(immediate)? range $min:literal ..= $max:literal $($rest:tt)*]
    ) => {
        if $name == $k {
            return Some(FieldKind::FloatRange($min, $max));
        }
    };
    (kind, $name:ident, [$(#[$doc:meta])* str $f:ident $k:literal $($rest:tt)*]) => {
        if $name == $k {
            return Some(FieldKind::Str);
        }
    };
    (kind, $name:ident, [$(#[$doc:meta])* free_str $f:ident $k:literal $($rest:tt)*]) => {
        if $name == $k {
            return Some(FieldKind::FreeStr);
        }
    };
    (kind, $name:ident, [$(#[$doc:meta])* opt_str $f:ident $k:literal $($rest:tt)*]) => {
        if $name == $k {
            return Some(FieldKind::OptStr);
        }
    };
    (
        kind, $name:ident,
        [$(#[$doc:meta])* enum $f:ident $k:literal $ty:ident $(label $l:literal)? $(help $h:literal)? $(immediate)?]
    ) => {
        if $name == $k {
            return Some(FieldKind::Enum($ty::SPELLINGS));
        }
    };
    (
        kind, $name:ident,
        [$(#[$doc:meta])* provider $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $name == $k {
            return Some(FieldKind::Provider);
        }
    };
    (kind, $name:ident, [$(#[$doc:meta])* stages $f:ident $k:literal $($rest:tt)*]) => {
        if $name == $k {
            return Some(FieldKind::Composite);
        }
    };
    // The effective value.
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* bool $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Bool($settings.$f));
        }
    };
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* uint $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Uint($settings.$f));
        }
    };
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* positive_uint $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Uint($settings.$f));
        }
    };
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* float_range $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Float($settings.$f));
        }
    };
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* str $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Str($settings.$f.clone()));
        }
    };
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* free_str $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Str($settings.$f.clone()));
        }
    };
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* opt_str $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Str($settings.$f.clone().unwrap_or_default()));
        }
    };
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* enum $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Str($settings.$f.as_str().into()));
        }
    };
    (
        value, $settings:ident, $field:ident,
        [$(#[$doc:meta])* provider $f:ident $k:literal $($rest:tt)*]
    ) => {
        if $field == $k {
            return Some(SettingValue::Str($settings.$f.as_str().into()));
        }
    };
    // The effective value of a composite key cannot be a single value: skip.
    (value, $settings:ident, $field:ident, [$($rest:tt)*]) => {};
    // The overlay help.
    (
        help, $key:ident,
        [$(#[$doc:meta])* enum $f:ident $k:literal $ty:ident $(label $l:literal)? help $h:literal $($rest:tt)*]
    ) => {
        if $key == $k {
            return Some($h);
        }
    };
    (
        help, $key:ident,
        [$(#[$doc:meta])* $kind:ident $f:ident $k:literal $(label $l:literal)? help $h:literal $($rest:tt)*]
    ) => {
        if $key == $k {
            return Some($h);
        }
    };
    // A key with no help entry contributes nothing.
    (help, $key:ident, [$($rest:tt)*]) => {};
    // The overlay label.
    (
        label, $key:ident,
        [$(#[$doc:meta])* enum $f:ident $k:literal $ty:ident label $l:literal $($rest:tt)*]
    ) => {
        if $key == $k {
            return Some($l);
        }
    };
    (
        label, $key:ident,
        [$(#[$doc:meta])* $kind:ident $f:ident $k:literal label $l:literal $($rest:tt)*]
    ) => {
        if $key == $k {
            return Some($l);
        }
    };
    // A key with no label entry contributes nothing.
    (label, $key:ident, [$($rest:tt)*]) => {};
}

/// The table driver: takes the struct-level docs, the entries and the normalization
/// block, and emits every derived item.
macro_rules! daemon_table {
    (
        $(#[$sdoc:meta])*
        entries { $( $entry:tt ),* $(,)? }
        normalize $settings:ident { $($norm:tt)* }
    ) => {
        daemon_settings! {
            docs [ $(#[$sdoc])* ]
            fields []
            $( $entry )*
        }

        impl Default for DaemonSettings {
            fn default() -> Self {
                materialize(DaemonLayer::default())
            }
        }

        daemon_layer! {
            fields []
            $( $entry )*
        }

        /// Merges two daemon layers, `higher` winning per key: structured fields merge
        /// deeper (the stage list by stage), plain
        /// values are replaced wholesale.
        fn merge(lower: DaemonLayer, higher: DaemonLayer) -> DaemonLayer {
            daemon_merge! {
                merged []
                lower, higher,
                $( $entry )*
            }
        }

        /// Parses one file's `[daemon]` table into a raw layer. Wrong types fall back
        /// to the field's default with a warning; unknown keys warn; unrecognized
        /// provider values push hard errors onto `errors` (the field still falls
        /// back to its default).
        pub(crate) fn parse_layer(
            table: &toml::Table,
            path: &Path,
            warnings: &mut Vec<String>,
            errors: &mut Vec<String>,
        ) -> DaemonLayer {
            let mut layer = DaemonLayer::default();
            let mut f = Fields {
                path,
                schema: "daemon",
                warnings,
            };
            for (key, value) in table {
                $(
                    daemon_entry! { parse, layer, f, key, value, errors, $entry }
                )*
                f.unknown(key);
            }
            layer
        }

        /// Turns the merged raw layer into the final settings: hardcoded defaults for
        /// unset fields, then the table's normalization.
        pub(crate) fn materialize(layer: DaemonLayer) -> DaemonSettings {
            let $settings = daemon_materialize! {
                settings []
                layer,
                $( $entry )*
            };
            $settings
        }

        /// Every non-composite `[daemon]` key, in table order: exactly the fields the
        /// settings-change flow can carry as a single value. The composite key (`stages`)
        /// has no single-value shape, so the diff skips it.
        ///
        /// Part of the cross-crate drift surface of finding 10: iterating this list,
        /// `patok-tui`'s drift test asserts every registry key has an overlay row and
        /// a `to_proto` case, so a field missing anywhere fails that test instead of
        /// drifting silently.
        pub const DAEMON_FIELDS: &[&str] = daemon_names! {
            keys []
            $( $entry )*
        };

        /// Daemon fields read at the point of use, so a change applies immediately:
        /// everything else parameterizes a unit of work and takes effect at the next
        /// such start. A new field joins whichever list matches how it is actually
        /// consumed (the `immediate` marker of its table entry).
        pub(crate) const IMMEDIATE_DAEMON_FIELDS: &[&str] = daemon_immediate! {
            keys []
            $( $entry )*
        };

        /// The daemon field registry: every `[daemon]` key and what it accepts.
        ///
        /// Part of the cross-crate drift surface of finding 10: the settings overlay
        /// and both proto mappers read a key's kind through this lookup, and the
        /// drift test in `patok-tui` asserts their coverage of every
        /// [`DAEMON_FIELDS`] key.
        pub fn daemon_field(name: &str) -> Option<FieldKind> {
            $(
                daemon_entry! { kind, name, $entry }
            )*
            None
        }

        /// The field's effective (post-merge, post-normalization) value, as reported
        /// back with a [`crate::ApplyTiming`] broadcast; `None` for a name
        /// outside the registry or a composite key.
        pub fn daemon_field_value(
            settings: &DaemonSettings,
            field: &str,
        ) -> Option<SettingValue> {
            $(
                daemon_entry! { value, settings, field, $entry }
            )*
            None
        }

        /// The help text of one settings-overlay field key, from its table entry;
        /// `None` for a key with no help text.
        fn field_help(key: &str) -> Option<&'static str> {
            $(
                daemon_entry! { help, key, $entry }
            )*
            None
        }

        /// The settings-overlay row label of one daemon field key, from its table
        /// entry; `None` for a key with no label (the composite keys carry none).
        ///
        /// Part of the cross-crate drift surface of finding 10: the settings overlay
        /// builds each daemon row's label through this lookup, so the label lives
        /// with the field's kind, default and help instead of a hand-written row.
        pub fn daemon_label(key: &str) -> Option<&'static str> {
            $(
                daemon_entry! { label, key, $entry }
            )*
            None
        }
    };
}
daemon_table! {
    /// The merged, normalized daemon settings. Every field here is read by the engine
    /// or shell; add a field only together with its consumer.
    entries {
        // Routing (section 4).
        [str research_model "research_model" label "research model" help "The model the research role runs on; empty uses the provider's default." default String::new()],
        [str planner_model "planner_model" label "planner model" help "The model the planner role runs on; empty uses the provider's default." default String::new()],
        [str builder_model "builder_model" label "builder model" help "The model the builder role runs on; empty uses the provider's default." default String::new()],
        [str reviewer_model "reviewer_model" label "reviewer model" help "The model the reviewer role runs on; empty uses the provider's default." default String::new()],
        [str discovery_model "discovery_model" label "discovery model" help "The model the discovery role runs on; empty uses the provider's default." default String::new()],
        [str orchestrator_model "orchestrator_model" label "orchestrator model" help "The model the orchestrator role runs on; empty uses the provider's default." default String::new()],
        [provider research_provider "research_provider" label "research provider" help "The provider the research role runs on. Applies at the next research session start."],
        [provider planner_provider "planner_provider" label "planner provider" help "The provider the planner role runs on. Applies at the next planner session start."],
        [provider builder_provider "builder_provider" label "builder provider" help "The provider the builder role runs on. Applies at the next builder session start."],
        [provider reviewer_provider "reviewer_provider" label "reviewer provider" help "The provider the reviewer role runs on. Applies at the next reviewer session start."],
        [provider discovery_provider "discovery_provider" label "discovery provider" help "The provider the discovery role runs on. Applies at the next discovery session start."],
        [provider orchestrator_provider "orchestrator_provider" label "orchestrator provider" help "The provider the orchestrator role runs on. Applies at the next orchestrator session start."],
        // Pipeline flow (section 5).
        [enum run_mode "run_mode" RunMode label "run mode" help "sprint (default) or continuous. The running loop keeps the mode it started with; a change applies at the next run."],
        [stages stages "stages" help "The pipeline stage list. Only the review stage can be disabled: enabled = false on research, plan or build is ignored with a warning, and those stages still run."],
        [/// Whether a Simple-classified task skips the research stage (T68.1).
         bool skip_research_for_simple "skip_research_for_simple" label "skip research for simple" help "Simple tasks skip the research stage (default on)." default true],
        [bool skip_review_for_simple "skip_review_for_simple" label "skip review for simple" help "Simple tasks skip the review stage, only when the builder's verification exited cleanly (default on)." default true],
        [uint review_confidence_threshold "review_confidence_threshold" label "review confidence threshold" help "Consecutive review passes needed before learned confidence may skip the review for matching task shapes (simple and medium tasks only); 0 disables. Default 5." expect "a whole number" default 5],
        [bool batch_review "batch_review" label "batch review" help "Skip the review for all but the last task in each contiguous run of same-numbered task IDs; that task's review covers every commit in the run (default on)." default true],
        [bool review_in_loop "review_in_loop" label "review in loop" help "Run the reviewer stage; when off, a task counts as validated by the builder's verification alone (default on)." default true],
        [uint review_multipass_threshold "review_multipass_threshold" label "review multipass threshold" help "Above this many changed files the reviewer splits into per-file passes plus one integration pass; 0 disables. Default 0." expect "a whole number" default 0],
        [float_range confidence_threshold "confidence_threshold" label "confidence threshold" help "Findings below this confidence are logged for manual review instead of being auto-fixed. Range 0.0-1.0, default 0.5." range 0.0..=1.0 default 0.5],
        [bool auto_push_enabled "auto_push_enabled" label "auto-push enabled" help "When on, push HEAD to auto_push_remote (git push <remote> HEAD) after each completed task group (default off)." immediate default false],
        [free_str auto_push_remote "auto_push_remote" label "auto-push remote" help "The remote pushed to (git push <remote> HEAD) after each completed task group when auto-push is enabled; empty means no push. Default origin." immediate default "origin".into()],
        [positive_uint discovery_cooldown_secs "discovery_cooldown_secs" label "discovery cooldown" help "Minimum time between discovery rounds, in seconds. Default 300 (5 minutes)." expect "a whole number of seconds greater than zero" default super::DEFAULT_DISCOVERY_COOLDOWN_SECS],
        [positive_uint discovery_cooldown_cap_secs "discovery_cooldown_cap_secs" label "discovery cooldown cap" help "The discovery cooldown doubles after each round, up to this cap in seconds. Default 1800 (30 minutes)." expect "a whole number of seconds greater than zero" default super::DEFAULT_DISCOVERY_COOLDOWN_CAP_SECS],
        [/// Whether a plan session runs before each builder session (T10.1).
         bool plan_enabled "plan_enabled" label "plan enabled" help "Whether a plan session runs before each builder session." default true],
        // Timing (section 6).
        [positive_uint agent_timeout_secs "agent_timeout_secs" label "agent timeout" help "How long one agent invocation may idle, in seconds, before it is stopped; the hard timeout is four times this. Default 600." expect "a whole number of seconds greater than zero" default 600],
        [uint pause_between_tasks_secs "pause_between_tasks_secs" label "pause between tasks" help "Pause between two tasks, in seconds. Default 10." expect "a whole number of seconds" default 10],
        [uint pause_between_agents_secs "pause_between_agents_secs" label "pause between agents" help "Pause between two agent invocations, in seconds. Default 3." expect "a whole number of seconds" default 3],
        [uint pause_between_cycles_secs "pause_between_cycles_secs" label "pause between cycles" help "Pause between build cycles, in seconds. Default 30." expect "a whole number of seconds" default 30],
        [bool adaptive_pauses "adaptive_pauses" label "adaptive pauses" help "Shorten the pauses to about 500 ms when the last agent was not rate limited (default on)." default true],
        [uint engine_idle_timeout_secs "engine_idle_timeout_secs" label "engine idle timeout" help "When no shell is attached and the engine has been idle this long, it shuts itself down. Default 1800 (30 minutes)." expect "a whole number of seconds" default 1800],
        // History (section 8).
        [opt_str history_dir "history_dir" label "history dir" help "Root directory of the session history (per-session and per-task logs); empty uses history/ in the project data directory."],
        [uint history_retention_tasks "history_retention_tasks" label "history retention tasks" help "How many tasks the history keeps. Default 1000." expect "a whole number" default 1000],
        [uint history_retention_days "history_retention_days" label "history retention days" help "How many days the history keeps its task records. Default 30." expect "a whole number" default 30],
    }
    normalize settings {
    }
}

impl Fields<'_> {
    /// The pipeline stage list (`[[daemon.stages]]`): merged by stage across layers. An
    /// entry whose `id` is unusable or names no accepted stage id is skipped with a
    /// warning; each of its fields with a wrong type falls back to that field's
    /// default.
    fn stages(&mut self, value: &toml::Value) -> Option<Vec<RawStage>> {
        let Some(entries) = value.as_array() else {
            self.warnings.push(format!(
                "{}: {}.stages must be an array of stage tables, got {value}; using the default",
                self.path.display(),
                self.schema
            ));
            return None;
        };
        let mut stages = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let Some(table) = entry.as_table() else {
                self.warnings.push(format!(
                    "{}: {}.stages entry {index} must be a table, got {entry}; skipping it",
                    self.path.display(),
                    self.schema
                ));
                continue;
            };
            let Some(id) = table
                .get("id")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                self.warnings.push(format!(
                    "{}: {}.stages entry {index} must have a non-empty string `id`; skipping it",
                    self.path.display(),
                    self.schema
                ));
                continue;
            };
            let Some(stage) = parse_stage_id(id) else {
                self.warnings.push(format!(
                    "{}: {}.stages entry {index} must name a pipeline stage ({ACCEPTED_STAGE_IDS}), got {id}; skipping it",
                    self.path.display(),
                    self.schema
                ));
                continue;
            };
            let mut entry = RawStage {
                stage,
                label: None,
                enabled: None,
                prompt: None,
            };
            for (field, value) in table {
                match field.as_str() {
                    "id" => {}
                    "label" => {
                        entry.label = self.string(&format!("stages entry `{id}` label"), value)
                    }
                    "enabled" => {
                        entry.enabled = self.boolean(&format!("stages entry `{id}` enabled"), value)
                    }
                    "prompt" => {
                        entry.prompt = self.string(&format!("stages entry `{id}` prompt"), value)
                    }
                    other => self.unknown(&format!("stages entry `{id}` key `{other}`")),
                }
            }
            stages.push(entry);
        }
        Some(stages)
    }
}

/// The four default pipeline stages.
fn default_stages() -> Vec<StageConfig> {
    [
        (Stage::Research, "RESEARCH"),
        (Stage::Plan, "PLAN"),
        (Stage::Build, "BUILD"),
        (Stage::Review, "REVIEW"),
    ]
    .into_iter()
    .map(|(stage, label)| StageConfig {
        stage,
        label: label.into(),
        enabled: true,
        prompt: None,
    })
    .collect()
}

/// The stage list merges by stage: a higher entry
/// overrides only that stage's fields, a stage a file omits keeps the lower definition,
/// and a stage a higher layer introduces is appended after the lower list.
fn merge_stages(
    lower: Option<Vec<RawStage>>,
    higher: Option<Vec<RawStage>>,
) -> Option<Vec<RawStage>> {
    match (lower, higher) {
        (None, None) => None,
        (Some(lower), None) => Some(lower),
        (lower, Some(higher)) => {
            let mut merged = lower.unwrap_or_default();
            for stage in higher {
                if let Some(existing) = merged.iter_mut().find(|s| s.stage == stage.stage) {
                    existing.label = stage.label.or(existing.label.take());
                    existing.enabled = stage.enabled.or(existing.enabled.take());
                    existing.prompt = stage.prompt.or(existing.prompt.take());
                } else {
                    merged.push(stage);
                }
            }
            Some(merged)
        }
    }
}

/// The daemon-schema environment overrides: the agent
/// timeout, under two variable names (the first valid one wins).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonEnv {
    /// [`PATOK_AGENT_TIMEOUT_SECS`] and its alias [`PATOK_AGENT_TIMEOUT`].
    pub agent_timeout_secs: [Option<String>; 2],
}

impl DaemonEnv {
    pub const AGENT_TIMEOUT_VARS: [&'static str; 2] =
        ["PATOK_AGENT_TIMEOUT_SECS", "PATOK_AGENT_TIMEOUT"];

    pub fn from_process_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self {
            agent_timeout_secs: [
                var(Self::AGENT_TIMEOUT_VARS[0]),
                var(Self::AGENT_TIMEOUT_VARS[1]),
            ],
        }
    }
}

/// The result of loading the daemon schema.
#[derive(Clone, Debug, Default)]
pub struct DaemonConfig {
    pub settings: DaemonSettings,
    /// Skip-and-fallback warnings; the caller logs them.
    pub warnings: Vec<String>,
    /// Hard errors that must refuse runs: unrecognized provider values.
    pub errors: Vec<String>,
}

/// Loads and merges the daemon schema across the four file layers and the environment.
/// The project layer lives in `data_dir` (the project's slot of the projects dir); a
/// legacy project-root config is migrated there first.
pub fn load_daemon(
    files: &ConfigFiles,
    project_dir: &Path,
    data_dir: &Path,
    env: &DaemonEnv,
) -> DaemonConfig {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    migrate_project_file(project_dir, data_dir, &mut warnings);
    let merged = load_layers(
        files,
        Some(&project_file(data_dir)),
        "daemon",
        "tui",
        &mut warnings,
        |table, path, warnings| parse_layer(table, path, warnings, &mut errors),
        merge,
    );
    let mut settings = materialize(merged);
    // D2: only review enablement is consumed (`review_stage_enabled`), so a
    // disabled research, plan or build entry is inert; warn instead of silently
    // ignoring it.
    for entry in &settings.stages {
        if entry.stage != Stage::Review && !entry.enabled {
            let id = entry.stage.as_str();
            warnings.push(format!(
                "ignoring daemon.stages `{id}` enabled = false: only the review stage can be disabled; the {id} stage still runs"
            ));
        }
    }
    apply_env(&mut settings, env, &mut warnings);
    DaemonConfig {
        settings,
        warnings,
        errors,
    }
}

/// The help text for one settings-overlay key of the daemon schema (T85.1):
/// the description of a setting row, or of a whole overlay section when the
/// key is a section's group id (`provider_and_model`, `pipeline`,
/// `timeouts_and_pauses`, `git`, `learnings_history`). The per-field
/// texts come from the field table's `help` entries (a composite key such as
/// `stages` carries its help there even though the overlay shows no row for
/// it), the group and read-only ones are hardcoded here, on the side that owns
/// the settings it describes; the shell routes to it by the row's declared
/// schema. `None` for a key that carries no help anywhere.
pub fn daemon_help(key: &str) -> Option<&'static str> {
    if let Some(help) = field_help(key) {
        return Some(help);
    }
    Some(match key {
        "provider_and_model" => {
            "Which CLI provider and model the agent roles run on. A change applies at the next task or agent start; whatever is already running keeps the value it started with."
        }
        "pipeline" => {
            "How tasks flow through the pipeline: which stages may be skipped for simple tasks, how the reviewer is scheduled, and how its findings are triaged."
        }
        "timeouts_and_pauses" => {
            "Timing: how long an agent may idle, the pauses between tasks, agents and cycles, and how long an engine with no shell attached waits before shutting down."
        }
        "git" => "How finished work reaches git.",
        "learnings_history" => {
            "History: where the per-session and per-task logs live and how long they are kept."
        }
        "engine_version" => "The engine's version, as reported by the daemon. Read-only.",
        "shell_version" => "The shell's own version. Read-only.",
        _ => return None,
    })
}

/// Applies the daemon-schema environment overrides: the agent
/// timeout, under two variable names (the first valid one winning); empty or invalid
/// values are ignored with a warning.
pub(crate) fn apply_env(
    settings: &mut DaemonSettings,
    env: &DaemonEnv,
    warnings: &mut Vec<String>,
) {
    let warn = |warnings: &mut Vec<String>, name: &str, value: &str| {
        warnings.push(format!("ignoring invalid {name}={value:?}"));
    };
    for (name, value) in DaemonEnv::AGENT_TIMEOUT_VARS
        .iter()
        .zip(&env.agent_timeout_secs)
    {
        let Some(value) = value else { continue };
        match value.trim().parse::<u64>() {
            Ok(secs) if secs > 0 => {
                settings.agent_timeout_secs = secs;
                break;
            }
            _ => warn(warnings, name, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SettingValue, set_daemon_field};
    use patok_providers::ProviderKind;

    /// The daemon schema loaded from a project-local config file holding `text`,
    /// written into the project's slot of the projects dir.
    fn load_project(text: &str) -> DaemonConfig {
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("patok.config.toml"), text).unwrap();
        load_daemon(
            &ConfigFiles {
                user_global: None,
                user_local: None,
            },
            project.path(),
            data.path(),
            &DaemonEnv::default(),
        )
    }

    #[test]
    fn skip_research_for_simple_parses_and_defaults_true() {
        let loaded = load_project("[daemon]\nskip_research_for_simple = false\n");
        assert!(!loaded.settings.skip_research_for_simple);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(DaemonSettings::default().skip_research_for_simple);
    }

    #[test]
    fn auto_push_enabled_defaults_off_while_the_remote_stays_origin() {
        assert!(!DaemonSettings::default().auto_push_enabled);
        let loaded = load_project("[daemon]\nrun_mode = \"sprint\"\n");
        assert!(!loaded.settings.auto_push_enabled);
        assert_eq!(loaded.settings.auto_push_remote, "origin");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let loaded = load_project("[daemon]\nauto_push_enabled = true\n");
        assert!(loaded.settings.auto_push_enabled);
    }

    #[test]
    fn auto_push_remote_defaults_to_origin_and_keeps_an_empty_value() {
        assert_eq!(DaemonSettings::default().auto_push_remote, "origin");
        let loaded = load_project("[daemon]\nauto_push_remote = \"\"\n");
        assert_eq!(loaded.settings.auto_push_remote, "");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let loaded = load_project("[daemon]\nauto_push_remote = \"backup\"\n");
        assert_eq!(loaded.settings.auto_push_remote, "backup");
    }

    #[test]
    fn skip_review_for_simple_parses_and_defaults_true() {
        let loaded = load_project("[daemon]\nskip_review_for_simple = false\n");
        assert!(!loaded.settings.skip_review_for_simple);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(DaemonSettings::default().skip_review_for_simple);
    }

    #[test]
    fn review_multipass_threshold_defaults_to_zero_and_parses() {
        assert_eq!(DaemonSettings::default().review_multipass_threshold, 0);
        let loaded = load_project("[daemon]\n");
        assert_eq!(loaded.settings.review_multipass_threshold, 0);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let loaded = load_project("[daemon]\nreview_multipass_threshold = 8\n");
        assert_eq!(loaded.settings.review_multipass_threshold, 8);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    }

    #[test]
    fn an_unknown_daemon_key_warns_without_refusing_runs() {
        // Keys removed from the schema (or simply mistyped) fall through to the
        // generic unknown-key arm: the config loads with defaults and a warning.
        let loaded = load_project("[daemon]\nreticulate_splines = true\n");
        assert_eq!(
            loaded.settings.reviewer_provider,
            DaemonSettings::default().reviewer_provider
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains("ignoring unknown key daemon.")),
            "{:?}",
            loaded.warnings
        );
    }

    #[test]
    fn builder_provider_codex_resolves_to_the_codex_kind() {
        let loaded = load_project("[daemon]\nbuilder_provider = \"codex\"\n");
        assert_eq!(loaded.settings.builder_provider, ProviderName::Codex);
        assert_eq!(
            loaded.settings.builder_provider.kind(),
            Some(ProviderKind::Codex)
        );
        assert_eq!(ProviderKind::Codex.as_str(), "codex");
    }

    /// The pre-rename `mistral` spelling still loads, as the `Vibe` harness.
    #[test]
    fn mistral_spelling_still_loads_as_vibe() {
        let loaded = load_project("[daemon]\nbuilder_provider = \"mistral\"\n");
        assert_eq!(loaded.settings.builder_provider, ProviderName::Vibe);
        assert_eq!(
            loaded.settings.builder_provider.kind(),
            Some(ProviderKind::Vibe)
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    }

    /// The aliases parse to their variants exactly like the canonical spellings:
    /// trimmed and case-insensitive; anything else is rejected.
    #[test]
    fn provider_aliases_parse_to_their_variants() {
        assert_eq!(ProviderName::parse("vibe"), Some(ProviderName::Vibe));
        assert_eq!(ProviderName::parse("mistral"), Some(ProviderName::Vibe));
        assert_eq!(ProviderName::parse(" Mistral "), Some(ProviderName::Vibe));
        assert_eq!(ProviderName::parse("CLAUDE"), Some(ProviderName::Claude));
        for removed in [
            "opencode",
            "ghcopilot",
            "gh-copilot",
            "github-copilot",
            "copilot",
        ] {
            assert_eq!(ProviderName::parse(removed), None, "{removed}");
        }
        assert_eq!(ProviderName::parse("not-a-provider"), None);
        assert_eq!(ProviderName::parse(""), None);
    }

    /// The derived accepted list names every canonical spelling followed by its
    /// aliases, in the order the persistence validator's error message shows.
    #[test]
    fn accepted_spellings_list_every_alias_after_its_variant() {
        assert_eq!(
            ProviderName::accepted().collect::<Vec<_>>(),
            ["claude", "codex", "vibe", "mistral"]
        );
    }

    /// A config naming a removed provider is a load error listing only the
    /// supported spellings, and the field falls back to the default.
    #[test]
    fn removed_providers_are_rejected_on_load() {
        for s in [
            "opencode",
            "ghcopilot",
            "gh-copilot",
            "github-copilot",
            "copilot",
        ] {
            let loaded = load_project(&format!("[daemon]\nbuilder_provider = \"{s}\"\n"));
            assert_eq!(loaded.settings.builder_provider, ProviderName::Claude);
            assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
            let expected = format!(
                "must be a provider name (claude, codex, vibe (alias mistral)), got \"{s}\""
            );
            assert!(loaded.errors[0].contains(&expected), "{:?}", loaded.errors);
        }
    }

    /// The generated prose names every spelling once, each variant's aliases in
    /// parentheses after it, plural only when there are several.
    #[test]
    fn accepted_spellings_prose_names_every_spelling_and_alias() {
        assert_eq!(
            ProviderName::ACCEPTED_SPELLINGS,
            "claude, codex, vibe (alias mistral)"
        );
    }

    /// The stage-id parser accepts every pipeline spelling plus the `implement`
    /// alias, trimmed and case-insensitive; anything else is rejected.
    #[test]
    fn stage_ids_parse_through_spellings_and_the_implement_alias() {
        assert_eq!(parse_stage_id("research"), Some(Stage::Research));
        assert_eq!(parse_stage_id("plan"), Some(Stage::Plan));
        assert_eq!(parse_stage_id("build"), Some(Stage::Build));
        assert_eq!(parse_stage_id("implement"), Some(Stage::Build));
        assert_eq!(parse_stage_id(" BUILD "), Some(Stage::Build));
        assert_eq!(parse_stage_id("Implement"), Some(Stage::Build));
        assert_eq!(parse_stage_id("review"), Some(Stage::Review));
        assert_eq!(parse_stage_id("doubt"), None);
        assert_eq!(parse_stage_id(""), None);
    }

    /// A valid stages list parses into stage configs keyed by the pipeline enum:
    /// the `implement` alias keys the Build stage, an unlabelled entry takes the
    /// canonical spelling as its label, and an absent `enabled` defaults to true.
    #[test]
    fn a_valid_stages_list_parses_into_stage_configs() {
        let loaded = load_project(
            r#"[daemon]
[[daemon.stages]]
id = "research"
label = "The research stage"
prompt = "custom prompt"
[[daemon.stages]]
id = "implement"
[[daemon.stages]]
id = " REVIEW "
enabled = false
[[daemon.stages]]
id = "plan"
"#,
        );
        assert_eq!(
            loaded.settings.stages,
            vec![
                StageConfig {
                    stage: Stage::Research,
                    label: "The research stage".into(),
                    enabled: true,
                    prompt: Some("custom prompt".into()),
                },
                StageConfig {
                    stage: Stage::Build,
                    label: "build".into(),
                    enabled: true,
                    prompt: None,
                },
                StageConfig {
                    stage: Stage::Review,
                    label: "review".into(),
                    enabled: false,
                    prompt: None,
                },
                StageConfig {
                    stage: Stage::Plan,
                    label: "plan".into(),
                    enabled: true,
                    prompt: None,
                },
            ]
        );
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);

        // The default list is the four pipeline stages, all enabled.
        let defaults = DaemonSettings::default().stages;
        assert_eq!(
            defaults.iter().map(|stage| stage.stage).collect::<Vec<_>>(),
            [Stage::Research, Stage::Plan, Stage::Build, Stage::Review]
        );
        assert!(defaults.iter().all(|stage| stage.enabled));
        assert_eq!(
            defaults
                .iter()
                .map(|stage| stage.label.as_str())
                .collect::<Vec<_>>(),
            ["RESEARCH", "PLAN", "BUILD", "REVIEW"]
        );
    }

    /// A stage id naming no pipeline stage is skipped with a warning (D1): exactly
    /// one warning naming the file, the entry index, the accepted ids and the
    /// offending value; the valid sibling entries still parse -- `implement`
    /// materializes as the Build stage -- and the load carries no errors, so runs
    /// are not refused.
    #[test]
    fn an_unknown_stage_id_warns_and_the_entry_is_skipped() {
        let loaded = load_project(
            r#"[daemon]
[[daemon.stages]]
id = "review"
enabled = false
[[daemon.stages]]
id = "implement"
[[daemon.stages]]
id = "doubt"
"#,
        );
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        let warning = &loaded.warnings[0];
        assert!(warning.contains("patok.config.toml"), "{warning}");
        assert!(warning.contains("daemon.stages entry 2"), "{warning}");
        assert!(warning.contains(ACCEPTED_STAGE_IDS), "{warning}");
        assert!(warning.contains("doubt"), "{warning}");
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(
            loaded.settings.stages,
            vec![
                StageConfig {
                    stage: Stage::Review,
                    label: "review".into(),
                    enabled: false,
                    prompt: None,
                },
                StageConfig {
                    stage: Stage::Build,
                    label: "build".into(),
                    enabled: true,
                    prompt: None,
                },
            ]
        );
    }

    /// A disabled research, plan or build entry warns at load time (D2), one
    /// warning per disabled stage naming the stage and the limit: only the
    /// review stage can be disabled. The entries load unchanged -- the values
    /// are not rewritten and the load carries no errors, so runs are not
    /// refused and which stages run is unchanged.
    #[test]
    fn a_disabled_research_plan_or_build_stage_warns_that_only_review_can_be_disabled() {
        let loaded = load_project(
            r#"[daemon]
[[daemon.stages]]
id = "research"
enabled = false
[[daemon.stages]]
id = "plan"
enabled = false
[[daemon.stages]]
id = "build"
enabled = false
"#,
        );
        assert_eq!(loaded.warnings.len(), 3, "{:?}", loaded.warnings);
        for (warning, id) in loaded.warnings.iter().zip(["research", "plan", "build"]) {
            assert!(warning.contains(id), "{warning}");
            assert!(
                warning.contains("only the review stage can be disabled"),
                "{warning}"
            );
        }
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(
            loaded.settings.stages,
            vec![
                StageConfig {
                    stage: Stage::Research,
                    label: "research".into(),
                    enabled: false,
                    prompt: None,
                },
                StageConfig {
                    stage: Stage::Plan,
                    label: "plan".into(),
                    enabled: false,
                    prompt: None,
                },
                StageConfig {
                    stage: Stage::Build,
                    label: "build".into(),
                    enabled: false,
                    prompt: None,
                },
            ]
        );
    }

    /// A config disabling the review stage loads with no warning (D2): review
    /// enablement is the one stage setting the engine consumes, so the
    /// documented disable is accepted silently. A stage a configured list
    /// omits (research, plan, build here) stays its default enabled state and
    /// warns nothing either.
    #[test]
    fn disabling_the_review_stage_loads_without_a_warning() {
        let loaded = load_project(
            r#"[daemon]
[[daemon.stages]]
id = "review"
enabled = false
"#,
        );
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(
            loaded.settings.stages,
            vec![StageConfig {
                stage: Stage::Review,
                label: "review".into(),
                enabled: false,
                prompt: None,
            }]
        );
    }

    /// The help registry (T85.1) answers every overlay key it owns with
    /// non-empty text, and nothing else.
    #[test]
    fn daemon_help_covers_its_keys_and_rejects_unknown_ones() {
        for key in [
            "builder_provider",
            "agent_timeout_secs",
            "auto_push_enabled",
            "auto_push_remote",
            "pipeline",
            "stages",
        ] {
            let help = daemon_help(key).unwrap_or_else(|| panic!("`{key}` has no help text"));
            assert!(!help.is_empty());
        }
        // The composite `stages` key carries its field-table help even though
        // the overlay shows no row for it; groups of the other schema stay out.
        assert_eq!(daemon_help("theme"), None);
    }

    /// Finding 10: every single-value registry key carries the settings-overlay row
    /// label the overlay builds its row from, so a new setting without one fails here
    /// instead of surfacing as a missing overlay label; the composite `stages` key carries none.
    #[test]
    fn every_daemon_field_key_has_an_overlay_label() {
        for &key in DAEMON_FIELDS {
            let label = daemon_label(key).unwrap_or_else(|| panic!("`{key}` has no label"));
            assert!(!label.is_empty(), "`{key}` has an empty label");
        }
        assert_eq!(daemon_label("stages"), None, "`stages` must carry no label");
    }

    /// The per-field sample a round trip writes and reads back: valid for the field's
    /// kind, and different from both the field's default readout and the lower
    /// layer's value.
    fn sample_value(kind: &FieldKind, default: &SettingValue) -> SettingValue {
        match kind {
            FieldKind::Bool => SettingValue::Bool(!default.as_bool().expect("a boolean default")),
            FieldKind::Uint | FieldKind::PositiveUint | FieldKind::Range(..) => {
                SettingValue::Uint(default.as_uint().expect("a whole-number default") + 1)
            }
            FieldKind::FloatRange(min, max) => SettingValue::Float(min + (max - min) * 3.0 / 4.0),
            FieldKind::Str | FieldKind::OptStr | FieldKind::FreeStr => {
                SettingValue::Str("round-trip-sample".into())
            }
            FieldKind::Enum(accepted) => SettingValue::Str(accepted[accepted.len() - 1].into()),
            FieldKind::Provider => SettingValue::Str("codex".into()),
            FieldKind::Composite => unreachable!("composite keys are not registry fields"),
        }
    }

    /// The lower layer's value for the same field: valid for the kind, different from
    /// the sample.
    fn lower_value(kind: &FieldKind, default: &SettingValue) -> SettingValue {
        match kind {
            FieldKind::Bool | FieldKind::Uint | FieldKind::PositiveUint | FieldKind::Range(..) => {
                default.clone()
            }
            FieldKind::FloatRange(min, max) => SettingValue::Float(min + (max - min) / 4.0),
            FieldKind::Str | FieldKind::OptStr | FieldKind::FreeStr => {
                SettingValue::Str("round-trip-lower".into())
            }
            FieldKind::Enum(accepted) => SettingValue::Str(accepted[0].into()),
            FieldKind::Provider => SettingValue::Str("claude".into()),
            FieldKind::Composite => unreachable!("composite keys are not registry fields"),
        }
    }

    /// The TOML spelling of a submitted value, as the right-hand side of a config line.
    fn toml_value(value: &SettingValue) -> String {
        match value {
            SettingValue::Bool(b) => format!("{b}"),
            SettingValue::Uint(u) => format!("{u}"),
            SettingValue::Float(f) => format!("{f}"),
            SettingValue::Str(s) => format!("{s:?}"),
            SettingValue::Unset => String::new(),
        }
    }

    #[test]
    fn every_daemon_field_parses_merges_and_persists_round_trip() {
        let defaults = DaemonSettings::default();
        for field in DAEMON_FIELDS {
            let kind = daemon_field(field).expect("every registry key has a kind");
            let default =
                daemon_field_value(&defaults, field).expect("every registry key has a value");
            let sample = sample_value(&kind, &default);
            assert_ne!(
                sample, default,
                "{field}: the sample must differ from the default"
            );

            // Persist: writing the sample through the settings-change flow and
            // reloading the file reports the sample back.
            let project = tempfile::tempdir().unwrap();
            let data = tempfile::tempdir().unwrap();
            let mut warnings = Vec::new();
            set_daemon_field(data.path(), field, sample.clone(), &mut warnings).unwrap();
            assert!(warnings.is_empty(), "{field}: {warnings:?}");
            let loaded = load_daemon(
                &ConfigFiles {
                    user_global: None,
                    user_local: None,
                },
                project.path(),
                data.path(),
                &DaemonEnv::default(),
            );
            assert!(loaded.warnings.is_empty(), "{field}: {:?}", loaded.warnings);
            assert!(loaded.errors.is_empty(), "{field}: {:?}", loaded.errors);
            assert_eq!(
                daemon_field_value(&loaded.settings, field),
                Some(sample.clone()),
                "{field}: the persisted sample round-trips"
            );

            // Merge: the user-local layer wins over the user-global one, and the
            // lower layer alone still carries its value.
            let user = tempfile::tempdir().unwrap();
            let lower = lower_value(&kind, &default);
            std::fs::write(
                user.path().join("config.toml"),
                format!("[daemon]\n{field} = {}\n", toml_value(&lower)),
            )
            .unwrap();
            std::fs::write(
                user.path().join("config.local.toml"),
                format!("[daemon]\n{field} = {}\n", toml_value(&sample)),
            )
            .unwrap();
            let files = ConfigFiles {
                user_global: Some(user.path().join("config.toml")),
                user_local: Some(user.path().join("config.local.toml")),
            };
            let loaded = load_daemon(
                &files,
                project.path(),
                tempfile::tempdir().unwrap().path(),
                &DaemonEnv::default(),
            );
            assert!(loaded.warnings.is_empty(), "{field}: {:?}", loaded.warnings);
            assert!(loaded.errors.is_empty(), "{field}: {:?}", loaded.errors);
            assert_eq!(
                daemon_field_value(&loaded.settings, field),
                Some(sample),
                "{field}: the higher layer wins"
            );
            let files = ConfigFiles {
                user_local: None,
                ..files
            };
            let loaded = load_daemon(
                &files,
                project.path(),
                tempfile::tempdir().unwrap().path(),
                &DaemonEnv::default(),
            );
            assert_eq!(
                daemon_field_value(&loaded.settings, field),
                Some(lower),
                "{field}: the lower layer survives alone"
            );
        }
    }

    /// The confidence-threshold range is enforced on the write path exactly like on
    /// the parse path, both reading the same bounds from the field table: the two
    /// messages name the same constraint.
    #[test]
    fn the_confidence_threshold_range_is_enforced_identically_on_both_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        for rejected in [1.5, -0.5] {
            let error = set_daemon_field(
                dir.path(),
                "confidence_threshold",
                SettingValue::Float(rejected),
                &mut warnings,
            )
            .unwrap_err();
            assert_eq!(
                error.message,
                format!(
                    "daemon.confidence_threshold must be a number between 0 and 1, got {rejected}"
                )
            );
            assert!(
                !dir.path().join("patok.config.toml").exists(),
                "the rejected value must not be written"
            );
        }
        // The inclusive bounds stay writable on the write path.
        for accepted in [0.0, 1.0] {
            set_daemon_field(
                dir.path(),
                "confidence_threshold",
                SettingValue::Float(accepted),
                &mut warnings,
            )
            .unwrap();
        }
        assert!(warnings.is_empty(), "{warnings:?}");

        // The parse path warns with the same constraint wording and falls back to
        // the default.
        let loaded = load_project("[daemon]\nconfidence_threshold = 1.5\n");
        let shared = "daemon.confidence_threshold must be a number between 0 and 1";
        let warning = loaded
            .warnings
            .iter()
            .find(|w| w.contains(shared))
            .expect("a range warning on the parse path");
        assert!(
            warning.contains(&format!("{shared}, got 1.5; using the default")),
            "{warning}"
        );
        assert_eq!(loaded.settings.confidence_threshold, 0.5);
    }
}
