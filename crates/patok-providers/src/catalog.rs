//! The static provider catalog: one entry per `ProviderKind` with its display names,
//! slug, CLI binary and capabilities.
//!
//! It lives in `patok-providers`, next to `ProviderKind`, so each provider fact is
//! written once here. Everything above this crate reads the catalog instead of keeping
//! its own copy. That includes `patok-config`, which depends on this crate for
//! `ProviderName`/`ProviderKind`, as well as the TUI, engine and wire. Config cannot sit
//! below providers: the layering is `patok-core` <- `patok-providers` <- `patok-config`,
//! so provider types can't be named in config while config lives in core.
//!
//! Models are deliberately not listed. A later phase discovers them by asking each
//! harness, so a hard-coded model list (or placeholder fields for one) would only go
//! stale. Add model data when discovery lands, not before.

use crate::ProviderKind;

/// The static facts about one provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderInfo {
    /// The kind this entry describes.
    pub kind: ProviderKind,
    /// The company behind the models, e.g. `Anthropic`.
    pub provider_name: &'static str,
    /// The agent harness patok drives, e.g. `Claude Code`.
    pub harness_name: &'static str,
    /// Short provider identifier, equal to the kind's config spelling, e.g. `claude`.
    pub slug: &'static str,
    /// Name of the CLI binary looked up on `PATH`.
    pub binary: &'static str,
    /// Whether the harness accepts a model choice. The engine passes a role's
    /// `<role>_model` only to providers that do (see `Provider::supports_model_selection`).
    pub supports_model_selection: bool,
}

/// Anthropic's Claude Code CLI.
pub(crate) const CLAUDE: ProviderInfo = ProviderInfo {
    kind: ProviderKind::Claude,
    provider_name: "Anthropic",
    harness_name: "Claude Code",
    slug: "claude",
    binary: "claude",
    supports_model_selection: true,
};

/// OpenAI's Codex CLI.
pub(crate) const CODEX: ProviderInfo = ProviderInfo {
    kind: ProviderKind::Codex,
    provider_name: "OpenAI",
    harness_name: "Codex",
    slug: "codex",
    binary: "codex",
    supports_model_selection: true,
};

/// Mistral's Vibe CLI (config alias `mistral`, see `ProviderName`).
pub(crate) const VIBE: ProviderInfo = ProviderInfo {
    kind: ProviderKind::Vibe,
    provider_name: "Mistral",
    harness_name: "Vibe",
    slug: "vibe",
    binary: "vibe",
    supports_model_selection: false,
};

/// Every provider, one entry per `ProviderKind`, in `ProviderKind` declaration order.
pub const CATALOG: &[ProviderInfo] = &[CLAUDE, CODEX, VIBE];

impl ProviderKind {
    /// This kind's catalog entry.
    pub fn info(self) -> &'static ProviderInfo {
        CATALOG
            .iter()
            .find(|info| info.kind == self)
            .expect("CATALOG lists every ProviderKind")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Provider;
    use crate::cli::CliSpec;
    use crate::{claude, codex, vibe};
    use patok_core::settings::EnumSpelling;

    #[test]
    fn every_kind_has_exactly_one_entry() {
        let variants = <ProviderKind as EnumSpelling>::VARIANTS;
        for (kind, _) in variants {
            assert_eq!(CATALOG.iter().filter(|i| i.kind == *kind).count(), 1);
        }
        assert_eq!(CATALOG.len(), variants.len());
    }

    #[test]
    fn slugs_match_the_kind_spellings() {
        for (kind, spelling) in <ProviderKind as EnumSpelling>::VARIANTS {
            assert_eq!(kind.info().slug, *spelling);
        }
    }

    #[test]
    fn providers_read_the_catalog() {
        assert_eq!(claude::Claude.slug(), CLAUDE.slug);
        assert_eq!(codex::Codex.slug(), CODEX.slug);
        assert_eq!(VIBE.provider_name, "Mistral");
        assert_eq!(VIBE.harness_name, "Vibe");
        assert_eq!(vibe::Vibe.slug(), VIBE.slug);
        assert_eq!(
            claude::Claude.supports_model_selection(),
            CLAUDE.supports_model_selection
        );
        assert_eq!(
            codex::Codex.supports_model_selection(),
            CODEX.supports_model_selection
        );
        assert_eq!(
            vibe::Vibe.supports_model_selection(),
            VIBE.supports_model_selection
        );
        assert_eq!(<codex::Codex as CliSpec>::INFO, &CODEX);
        assert_eq!(<vibe::Vibe as CliSpec>::INFO, &VIBE);
    }

    #[test]
    fn binaries_are_the_provider_clis() {
        let facts: Vec<_> = CATALOG
            .iter()
            .map(|i| (i.binary, i.supports_model_selection))
            .collect();
        assert_eq!(facts, [("claude", true), ("codex", true), ("vibe", false)]);
    }
}
