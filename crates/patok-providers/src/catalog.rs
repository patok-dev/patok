//! The static provider catalog: one entry per `ProviderKind` with its display names,
//! slug, CLI binary and capabilities.

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
    /// Whether the engine routes the configured reviewer model to this provider
    /// (see `Provider::supports_reviewer_model`).
    pub supports_reviewer_model: bool,
}

pub(crate) const CLAUDE: ProviderInfo = ProviderInfo {
    kind: ProviderKind::Claude,
    provider_name: "Anthropic",
    harness_name: "Claude Code",
    slug: "claude",
    binary: "claude",
    supports_reviewer_model: true,
};

pub(crate) const CODEX: ProviderInfo = ProviderInfo {
    kind: ProviderKind::Codex,
    provider_name: "OpenAI",
    harness_name: "Codex",
    slug: "codex",
    binary: "codex",
    supports_reviewer_model: false,
};

// The binary is `vibe`; the slug stays `mistral` until the kind is renamed.
pub(crate) const MISTRAL: ProviderInfo = ProviderInfo {
    kind: ProviderKind::Mistral,
    provider_name: "Mistral",
    harness_name: "Vibe",
    slug: "mistral",
    binary: "vibe",
    supports_reviewer_model: false,
};

/// Every provider, one entry per `ProviderKind`, in `ProviderKind` declaration order.
pub const CATALOG: &[ProviderInfo] = &[CLAUDE, CODEX, MISTRAL];

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
        assert_eq!(vibe::Vibe.slug(), MISTRAL.slug);
        assert_eq!(
            claude::Claude.supports_reviewer_model(),
            CLAUDE.supports_reviewer_model
        );
        assert_eq!(
            codex::Codex.supports_reviewer_model(),
            CODEX.supports_reviewer_model
        );
        assert_eq!(
            vibe::Vibe.supports_reviewer_model(),
            MISTRAL.supports_reviewer_model
        );
        assert_eq!(<codex::Codex as CliSpec>::INFO, &CODEX);
        assert_eq!(<vibe::Vibe as CliSpec>::INFO, &MISTRAL);
    }

    #[test]
    fn binaries_are_the_provider_clis() {
        let facts: Vec<_> = CATALOG
            .iter()
            .map(|i| (i.binary, i.supports_reviewer_model))
            .collect();
        assert_eq!(facts, [("claude", true), ("codex", false), ("vibe", false)]);
    }
}
