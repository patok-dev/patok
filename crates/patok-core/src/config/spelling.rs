//! The one shared spelling source of the config enums (finding 11 of the
//! structure review): the parse, `as_str` and `Display` behaviour the config
//! parsers, the persistence validator and the settings overlays all agree on,
//! generated from a single list of variant-and-spelling pairs per enum.

/// One enum whose variants each carry exactly one config spelling.
pub trait EnumSpelling: Copy + PartialEq + Sized + 'static {
    /// Every variant paired with its spelling, in declaration order: the one
    /// list the parser, the persistence validator, the overlay cycle and the
    /// warnings all derive from.
    const VARIANTS: &'static [(Self, &'static str)];
    /// The spellings alone, in the same order.
    const SPELLINGS: &'static [&'static str];

    /// Parses a config spelling (trimmed, case-insensitive); `None` for anything
    /// unrecognized.
    fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_lowercase();
        Self::VARIANTS
            .iter()
            .find(|(_, spelling)| normalized == **spelling)
            .map(|(variant, _)| *variant)
    }

    /// The variant's spelling; `VARIANTS` lists every variant.
    fn as_str(self) -> &'static str {
        Self::VARIANTS
            .iter()
            .find(|(variant, _)| *variant == self)
            .expect("VARIANTS lists every variant")
            .1
    }

    /// The accepted-spelling prose shared by the config warnings: "`a`, `b`".
    fn accepted_spellings() -> String {
        Self::SPELLINGS
            .iter()
            .map(|spelling| format!("`{spelling}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Defines one config enum together with its [`EnumSpelling`] implementation from a
/// single token list: the variants, their spellings, the inherent `parse`/`as_str`
/// forwarders and the `Display` behaviour (through the trait's blanket impl) all come
/// from the same place, so a variant cannot be added without a spelling and every
/// consumer reads the spellings through the trait instead of retyping literals.
macro_rules! config_enum {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident => $spelling:expr ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name { $( $(#[$variant_meta])* $variant ),+ }

        // Inherent forwarders keep every existing `Type::parse` / `value.as_str()`
        // call site resolving exactly as before.
        impl $name {
            /// Parses a config spelling (trimmed, case-insensitive); `None` for
            /// anything unrecognized.
            pub fn parse(value: &str) -> Option<Self> {
                $crate::config::EnumSpelling::parse(value)
            }

            /// The variant's config spelling.
            pub fn as_str(self) -> &'static str {
                $crate::config::EnumSpelling::as_str(self)
            }
        }

        impl $crate::config::EnumSpelling for $name {
            const VARIANTS: &'static [(Self, &'static str)] =
                &[ $( (Self::$variant, $spelling) ),+ ];
            const SPELLINGS: &'static [&'static str] = &[ $( $spelling ),+ ];

            // An exhaustive match: adding a variant without a spelling fails to
            // compile -- the compile-time exhaustiveness proof.
            fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $spelling ),+ }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
pub(crate) use config_enum;

/// Defines one config enum together with its [`EnumSpelling`] implementation
/// and its alias table from a single token list: the variants, their spellings,
/// their aliases, the inherent `parse`/`as_str` forwarders, the
/// `ACCEPTED_SPELLINGS` prose and the `Display` behaviour all come from the same
/// place, so a variant cannot be added without a spelling and an alias cannot
/// drift out of the parser or the prose. Each entry carries one spelling and at
/// most one alias list -- `[alias "x"]` for a single alias, `[aliases "x", "y"]`
/// for several; the chosen form decides "(alias ...)" versus "(aliases ...)" in
/// the prose.
macro_rules! config_enum_aliased {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $(#[$first_meta:meta])*
            $first_variant:ident => $first_spelling:expr
            $(, [alias $first_alias:literal])?
            $(, [aliases $first_many:literal $(, $first_rest:literal)*])?
            $(
                , $(#[$meta:meta])*
                $variant:ident => $spelling:expr
                $(, [alias $alias:literal])?
                $(, [aliases $alias_head:literal $(, $alias_tail:literal)*])?
            )+
            $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $(#[$first_meta])* $first_variant
            $(, $(#[$meta])* $variant)+
        }

        // Inherent forwarders keep every existing `Type::parse` / `value.as_str()`
        // call site resolving exactly as before.
        impl $name {
            /// Parses a config spelling: the canonical spellings, then the
            /// aliases; trimmed, case-insensitive; `None` for anything
            /// unrecognized.
            pub fn parse(value: &str) -> Option<Self> {
                $crate::config::EnumSpelling::parse(value).or_else(|| {
                    let normalized = value.trim().to_lowercase();
                    Self::ALIASES
                        .iter()
                        .find(|(alias, _)| normalized == *alias)
                        .map(|(_, variant)| *variant)
                })
            }

            /// The variant's config spelling.
            pub fn as_str(self) -> &'static str {
                $crate::config::EnumSpelling::as_str(self)
            }

            /// The config aliases, keyed by spelling: they parse like the
            /// canonical spellings and appear after each variant's spelling
            /// in [`Self::accepted`].
            pub const ALIASES: &[(&'static str, Self)] = &[
                $( ($first_alias, Self::$first_variant), )?
                $( ($first_many, Self::$first_variant), $( ($first_rest, Self::$first_variant), )* )?
                $(
                    $( ($alias, Self::$variant), )?
                    $( ($alias_head, Self::$variant), $( ($alias_tail, Self::$variant), )* )?
                )+
            ];

            /// The accepted-spelling prose of the config warnings: every
            /// spelling, each followed by its aliases in parentheses.
            pub const ACCEPTED_SPELLINGS: &str = concat!(
                $first_spelling
                $(, " (alias ", $first_alias, ")")?
                $(, " (aliases ", $first_many $(, ", ", $first_rest)*, ")")?
                $(
                    , ", ", $spelling
                    $(, " (alias ", $alias, ")")?
                    $(, " (aliases ", $alias_head $(, ", ", $alias_tail)*, ")")?
                )+
            );

            /// Every accepted spelling in the persistence validator's order:
            /// each variant's canonical spelling followed by its aliases.
            pub fn accepted() -> impl Iterator<Item = &'static str> {
                Self::VARIANTS.iter().flat_map(|&(variant, spelling)| {
                    ::std::iter::once(spelling).chain(
                        Self::ALIASES
                            .iter()
                            .filter(move |&&(_, aliased)| aliased == variant)
                            .map(|&(alias, _)| alias),
                    )
                })
            }
        }

        impl $crate::config::EnumSpelling for $name {
            const VARIANTS: &'static [(Self, &'static str)] = &[
                (Self::$first_variant, $first_spelling)
                $(, (Self::$variant, $spelling))+
            ];
            const SPELLINGS: &'static [&'static str] =
                &[$first_spelling $(, $spelling)+];

            // An exhaustive match: adding a variant without a spelling fails to
            // compile -- the compile-time exhaustiveness proof.
            fn as_str(self) -> &'static str {
                match self {
                    Self::$first_variant => $first_spelling
                    $(, Self::$variant => $spelling)+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
pub(crate) use config_enum_aliased;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AcceptPolicy, ProviderKind, ProviderName, RailMode, RunMode, Theme, UpdateChannel,
    };
    use crate::event::AgentRole;
    use crate::pipeline::Stage;
    use crate::review::Severity;
    use std::collections::HashSet;
    use std::fmt;

    /// The shared per-enum proof: every variant round-trips through `parse`,
    /// `as_str` and `Display` with its spelling; the parse is trim- and
    /// case-insensitive; unknowns (including the empty string) are rejected; the
    /// spelling list holds no duplicates; and the list is exhaustive -- enforced at
    /// compile time by the macro's generated exhaustive `as_str` match (a variant
    /// without a spelling cannot compile) and cross-checked here by the round-trip
    /// loop over `VARIANTS`.
    fn check<E>()
    where
        E: EnumSpelling + fmt::Display,
    {
        for &(variant, spelling) in E::VARIANTS {
            assert!(
                E::parse(spelling).is_some_and(|v| v == variant),
                "`{spelling}` does not round-trip"
            );
            assert_eq!(variant.as_str(), spelling);
            assert_eq!(variant.to_string(), spelling);
            let loud = format!("  {}  ", spelling.to_uppercase());
            assert!(
                E::parse(&loud).is_some_and(|v| v == variant),
                "`{loud}` does not round-trip"
            );
        }
        assert_eq!(E::VARIANTS.len(), E::SPELLINGS.len());
        assert_eq!(
            E::SPELLINGS.iter().collect::<HashSet<_>>().len(),
            E::SPELLINGS.len(),
            "a spelling appears twice"
        );
        assert!(E::parse("").is_none());
        assert!(E::parse("not-a-spelling").is_none());
    }

    #[test]
    fn provider_kind_round_trips_every_variant() {
        check::<ProviderKind>();
        assert_eq!(
            ProviderKind::accepted_spellings(),
            "`claude`, `codex`, `mistral`"
        );
    }

    #[test]
    fn provider_name_round_trips_every_variant() {
        check::<ProviderName>();
    }

    #[test]
    fn run_mode_round_trips_every_variant() {
        check::<RunMode>();
        assert_eq!(RunMode::accepted_spellings(), "`sprint`, `continuous`");
    }

    #[test]
    fn accept_policy_round_trips_every_variant() {
        check::<AcceptPolicy>();
        assert_eq!(
            AcceptPolicy::accepted_spellings(),
            "`no-high`, `no-high-medium`, `no-findings`"
        );
    }

    #[test]
    fn theme_round_trips_every_variant() {
        check::<Theme>();
        assert_eq!(
            Theme::accepted_spellings(),
            "`dark`, `atom_one_dark`, `atom_one_light`, `tokyo_night_dark`, `tokyo_night_day`, \
             `catppuccin_mocha`, `catppuccin_latte`, `solarized_dark`, `solarized_light`, \
             `gruvbox_dark`, `gruvbox_light`"
        );
    }

    #[test]
    fn update_channel_round_trips_every_variant() {
        check::<UpdateChannel>();
        assert_eq!(UpdateChannel::accepted_spellings(), "`stable`, `dev`");
    }

    #[test]
    fn rail_mode_round_trips_every_variant() {
        check::<RailMode>();
        assert_eq!(
            RailMode::accepted_spellings(),
            "`compact`, `normal`, `detailed`"
        );
    }

    #[test]
    fn agent_role_round_trips_every_variant() {
        check::<AgentRole>();
        // The settings' agent roles are a different family: their spellings
        // never parse as engine agent roles.
        assert!(AgentRole::parse("learning_extraction").is_none());
        assert!(AgentRole::parse("orchestrator_proposer").is_none());
    }

    #[test]
    fn severity_round_trips_every_variant() {
        check::<Severity>();
    }

    #[test]
    fn stage_round_trips_every_variant() {
        check::<Stage>();
    }
}
