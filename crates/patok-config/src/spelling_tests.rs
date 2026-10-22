//! Spelling proofs for the config enums this crate defines or uses.

use crate::{AcceptPolicy, RunMode, Severity, Theme};
use patok_core::settings::EnumSpelling;
use patok_providers::{ProviderKind, ProviderName};
use std::collections::HashSet;
use std::fmt;

// Duplicated from `patok_core::settings::spelling`'s tests: a `#[cfg(test)]` helper
// cannot be shared across crates without a test-support feature.
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
fn severity_round_trips_every_variant() {
    check::<Severity>();
}
