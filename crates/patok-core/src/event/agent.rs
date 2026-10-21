//! The normalised agent event vocabulary every provider translates its native stream into.

use serde::{Deserialize, Serialize};

use crate::settings::config_enum;

config_enum! {
    /// The engine's agent roles: the agents the engine announces as its active
    /// agent, the spellings the `agent` fields of
    /// [`crate::event::EngineEvent::AgentStarted`], [`crate::event::EngineEvent::AgentChanged`] and
    /// [`crate::event::EngineEvent::AgentFinished`] carry across the wire (`planner`,
    /// `research`, `discovery`, `builder`, `reviewer`). The engine converts to
    /// the spelling only at that boundary; `orchestrator` is themed today
    /// although no engine code announces it yet. This is a different family
    /// from the settings `Role` of the engine/TUI agent mappers.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum AgentRole {
        Planner => "planner",
        Research => "research",
        Discovery => "discovery",
        Builder => "builder",
        Reviewer => "reviewer",
        Orchestrator => "orchestrator",
    }
}

/// Token accounting of one agent session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub context_window: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

/// Typed error kinds recognised in provider error text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ErrorKind {
    ContextOverflow {
        requested: Option<u64>,
        available: Option<u64>,
    },
    ProviderUnreachable {
        url: Option<String>,
    },
    ModelNotLoaded {
        model: Option<String>,
    },
}

/// One item of the common event vocabulary every provider translates its native stream into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A complete assistant message or status line.
    Text {
        text: String,
    },
    /// An incremental chunk to append to the current line.
    TextDelta {
        text: String,
    },
    /// The agent's thinking/reasoning for one block, as markdown; rendered distinctly.
    Thinking {
        text: String,
    },
    /// Tool name plus a short input preview (up to 120 characters).
    ToolUse {
        name: String,
        input: String,
    },
    /// Output preview (up to 200 characters).
    ToolResult {
        output: String,
    },
    /// A raw error or warning line.
    Stderr {
        text: String,
    },
    /// The final answer text.
    Result {
        text: String,
    },
    Usage(Usage),
    Error {
        kind: ErrorKind,
        text: String,
    },
}

/// Truncates to at most `max` characters, marking the cut with an ellipsis.
pub fn preview(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Classifies error text into at most one typed kind, in priority order: context overflow,
/// provider unreachable, model not loaded.
pub fn classify_error(text: &str) -> Option<ErrorKind> {
    let lower = text.to_lowercase();
    if let Some((requested, available)) = parse_context_overflow(&lower) {
        return Some(ErrorKind::ContextOverflow {
            requested,
            available,
        });
    }
    if lower.contains("exceeds the available context size") || lower.contains("n_ctx_slot") {
        return Some(ErrorKind::ContextOverflow {
            requested: None,
            available: None,
        });
    }
    if ["connection refused", "econnrefused", "failed to connect"]
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some(ErrorKind::ProviderUnreachable {
            url: first_url(text),
        });
    }
    if ["model not loaded", "no model loaded", "model_not_found"]
        .iter()
        .any(|m| lower.contains(m))
        || (lower.contains("404") && lower.contains("/v1/models"))
    {
        return Some(ErrorKind::ModelNotLoaded { model: None });
    }
    None
}

/// Parses "request N tokens exceeds the available context size (M tokens)".
fn parse_context_overflow(lower: &str) -> Option<(Option<u64>, Option<u64>)> {
    let before = lower
        .split("tokens exceeds the available context size")
        .next()?;
    if before.len() == lower.len() {
        return None;
    }
    let requested = before.trim_end().rsplit(' ').next()?.parse().ok()?;
    let after = lower.split("context size (").nth(1)?;
    let available = after.split(' ').next()?.parse().ok()?;
    Some((Some(requested), Some(available)))
}

fn first_url(text: &str) -> Option<String> {
    let start = text.find("http://").or_else(|| text.find("https://"))?;
    let url = text[start..].split_whitespace().next()?;
    Some(
        url.trim_end_matches(['.', ',', ';', ':', ')', '"', '\''])
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_truncates_and_flattens() {
        assert_eq!(preview("a\n  b", 10), "a b");
        assert_eq!(preview("abcdef", 4), "abc…");
    }

    #[test]
    fn classifies_context_overflow_with_counts() {
        let kind =
            classify_error("Request 9000 tokens exceeds the available context size (4096 tokens)");
        assert_eq!(
            kind,
            Some(ErrorKind::ContextOverflow {
                requested: Some(9000),
                available: Some(4096)
            })
        );
        assert_eq!(
            classify_error("slot n_ctx_slot too small"),
            Some(ErrorKind::ContextOverflow {
                requested: None,
                available: None
            })
        );
    }

    #[test]
    fn classifies_unreachable_with_url_hint() {
        let kind = classify_error("failed to connect to http://127.0.0.1:1234/v1.");
        assert_eq!(
            kind,
            Some(ErrorKind::ProviderUnreachable {
                url: Some("http://127.0.0.1:1234/v1".into())
            })
        );
    }

    #[test]
    fn classifies_model_not_loaded_and_untyped() {
        assert!(matches!(
            classify_error("Model not loaded"),
            Some(ErrorKind::ModelNotLoaded { .. })
        ));
        assert!(matches!(
            classify_error("404 on /v1/models"),
            Some(ErrorKind::ModelNotLoaded { .. })
        ));
        assert_eq!(classify_error("something else"), None);
    }
}
