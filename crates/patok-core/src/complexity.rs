//! The task complexity classifier: every
//! task description is classified as Simple, Medium or Complex from the text
//! alone, before any stage runs. Only the Simple tier changes engine behaviour
//! today (the research stage's `skip_research_for_simple` gate); the rest is
//! the full classification kept for the stages that will consume it.

/// The action-verb list the distinct-verb signal counts from.
const ACTION_VERBS: [&str; 25] = [
    "add",
    "change",
    "update",
    "fix",
    "remove",
    "delete",
    "move",
    "create",
    "implement",
    "build",
    "write",
    "extend",
    "rename",
    "refactor",
    "migrate",
    "wire",
    "render",
    "show",
    "hide",
    "parse",
    "validate",
    "handle",
    "support",
    "improve",
    "document",
];

/// The first-token list of the Simple rule.
const SIMPLE_FIRST_TOKENS: [&str; 18] = [
    "rename", "add", "change", "update", "fix", "set", "remove", "delete", "move", "typo", "color",
    "label", "text", "value", "flag", "toggle", "bump", "version",
];

/// The bundling phrases, each counted once when present.
const BUNDLING_PHRASES: [&str; 6] = [
    "and also",
    "and additionally",
    " plus ",
    "two layers",
    "three layers",
    "four layers",
];

/// The risk-domain prefixes: a token equal to `auth` or beginning with one of
/// these adds 2 to the composite score.
const RISK_PREFIXES: [&str; 8] = [
    "authent",
    "authoriz",
    "secur",
    "encrypt",
    "payment",
    "migrat",
    "infrastructure",
    "schema",
];

/// The structural-keyword prefixes, +1 each capped at 2.
const STRUCTURAL_PREFIXES: [&str; 5] = ["architect", "redesign", "refactor", "rewrite", "overhaul"];

/// A task's complexity tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Complexity {
    Simple,
    Medium,
    Complex,
}

/// The signals the classifier extracted, plus the tier they produce. Returned
/// for explainability; the composite score is the sum of all signals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signals {
    pub tier: Complexity,
    /// Distinct "(n)" numbered sub-features.
    pub sub_features: usize,
    /// Distinct bundling phrases present.
    pub phrases: usize,
    /// Distinct action verbs in the description.
    pub verbs: usize,
    /// Distinct backtick-quoted path-like tokens.
    pub file_refs: usize,
    /// Risk-domain tokens hit (each adds 2).
    pub risk: usize,
    /// Structural keywords hit (each adds 1, capped at 2).
    pub structural: usize,
    /// The word-count length signal (0, 1 over 300 words, 2 over 500).
    pub length: usize,
}

impl Signals {
    /// The composite score: sub-features + phrases + the verb and file-ref
    /// thresholds + risk (x2) + structural + length.
    pub fn composite(&self) -> usize {
        self.sub_features
            + self.phrases
            + usize::from(self.verbs >= 3)
            + usize::from(self.file_refs > 6)
            + 2 * self.risk
            + self.structural
            + self.length
    }
}

/// Classifies `description` into Simple, Medium or Complex. A
/// `[fast]` flag as the first token forces Simple, a `[strict]` flag forces
/// Complex; both win over every signal. An empty description is Medium.
pub fn classify(description: &str) -> Complexity {
    extract(description).tier
}

/// Extracts the classification signals from `description` and applies the
/// decision rules. The `[fast]`/`[strict]` override flag is stripped from the
/// description before the signals are read, because the override is expected "as the
/// first token of the description".
pub fn extract(description: &str) -> Signals {
    let mut text = description.trim();
    let mut tier = None;
    let first = text.split_whitespace().next().unwrap_or_default();
    match first {
        "[fast]" => {
            tier = Some(Complexity::Simple);
            text = text.strip_prefix(first).unwrap_or(text).trim_start();
        }
        "[strict]" => {
            tier = Some(Complexity::Complex);
            text = text.strip_prefix(first).unwrap_or(text).trim_start();
        }
        _ => {}
    }
    let tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect();
    let words = text.split_whitespace().count();
    let lower = text.to_lowercase();

    // Distinct "(n)" numbered markers.
    let mut markers = std::collections::BTreeSet::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '(' {
            continue;
        }
        let mut digits = String::new();
        while let Some(&digit) = chars.peek() {
            if !digit.is_ascii_digit() {
                break;
            }
            digits.push(digit);
            chars.next();
        }
        if !digits.is_empty() && chars.peek() == Some(&')') {
            chars.next();
            markers.insert(digits);
        }
    }
    let sub_features = markers.len();
    // Each distinct bundling phrase counts once.
    let phrases = BUNDLING_PHRASES
        .iter()
        .filter(|phrase| lower.contains(**phrase))
        .count();
    // Distinct action verbs anywhere in the description.
    let verbs = tokens
        .iter()
        .filter(|token| ACTION_VERBS.contains(&token.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    // Distinct backtick-quoted path-like tokens (a slash or a dot makes it
    // path-like).
    let file_refs = text
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|quoted| quoted.contains('/') || quoted.contains('.'))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    // A token equal to "auth" or beginning with a risk-domain prefix.
    let risk = tokens
        .iter()
        .filter(|token| {
            token.as_str() == "auth" || RISK_PREFIXES.iter().any(|p| token.starts_with(p))
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    // Structural keywords, +1 each, capped at 2.
    let structural = tokens
        .iter()
        .filter(|token| STRUCTURAL_PREFIXES.iter().any(|p| token.starts_with(p)))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        .min(2);
    let length = usize::from(words > 300) + usize::from(words > 500);

    let signals = Signals {
        tier: Complexity::Medium,
        sub_features,
        phrases,
        verbs,
        file_refs,
        risk,
        structural,
        length,
    };
    let composite = signals.composite();
    let tier = tier.unwrap_or_else(|| {
        if composite >= 2 {
            Complexity::Complex
        } else if is_simple(text, &tokens, composite) {
            Complexity::Simple
        } else {
            // Medium is the default; an empty description lands here too.
            Complexity::Medium
        }
    });
    Signals { tier, ..signals }
}

/// The Simple rule: under 80 characters, the
/// first token exactly one of the verb/noun list, at most one distinct action
/// verb, and a composite score of 0.
fn is_simple(text: &str, tokens: &[String], composite: usize) -> bool {
    if composite != 0 || text.chars().count() >= 80 {
        return false;
    }
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    SIMPLE_FIRST_TOKENS.contains(&first)
        && tokens
            .iter()
            .filter(|token| ACTION_VERBS.contains(&token.as_str()))
            .count()
            <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_rename_is_simple() {
        assert_eq!(classify("rename the badge label"), Complexity::Simple);
    }

    #[test]
    fn a_payment_task_is_complex() {
        assert_eq!(
            classify("Add the payment confirmation screen"),
            Complexity::Complex
        );
        // A single risk-domain token reaches the threshold on its own.
        assert_eq!(classify("fix the auth flow"), Complexity::Complex);
        // "author" is not a risk-domain token.
        assert_eq!(classify("update the author byline"), Complexity::Simple);
    }

    #[test]
    fn a_numbered_marker_is_not_simple() {
        // One "(1)" marker is a sub-feature, so the composite score is 1: not
        // Simple (and not Complex), so Medium.
        assert_eq!(classify("add (1) the badge label"), Complexity::Medium);
    }

    #[test]
    fn two_bundling_signals_are_complex() {
        assert_eq!(
            classify("add the badge and also rework the header plus the footer area"),
            Complexity::Complex
        );
    }

    #[test]
    fn a_structural_rewrite_is_complex() {
        // refactor + rewrite hit the structural cap of 2.
        assert_eq!(
            classify("refactor the parser and rewrite the engine loader"),
            Complexity::Complex
        );
    }

    #[test]
    fn a_long_description_adds_length_signals() {
        let words = |n: usize| {
            format!("{} {}", "add", "word ".repeat(n))
                .trim_end()
                .to_string()
        };
        assert_eq!(classify(&words(0)), Complexity::Simple);
        // Over 300 words adds 1 (Medium); over 500 adds 2 (Complex).
        assert_eq!(classify(&words(301)), Complexity::Medium);
        assert_eq!(classify(&words(501)), Complexity::Complex);
    }

    #[test]
    fn a_substantial_task_is_medium() {
        // "write" is not in the Simple first-token list, so this is the default
        // tier for well-composed work.
        assert_eq!(classify("write the greeting file"), Complexity::Medium);
        assert_eq!(classify(""), Complexity::Medium);
    }

    #[test]
    fn the_fast_and_strict_flags_override_every_signal() {
        assert_eq!(
            classify("[fast] add a payment provider"),
            Complexity::Simple
        );
        assert_eq!(
            classify("[strict] rename the badge label"),
            Complexity::Complex
        );
    }

    #[test]
    fn many_file_references_add_a_signal() {
        let refs = (1..=7)
            .map(|n| format!("`src/{n}.rs`"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(classify(&format!("update {refs}")), Complexity::Medium);
        let refs = (1..=6)
            .map(|n| format!("`src/{n}.rs`"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(classify(&format!("update {refs}")), Complexity::Simple);
    }
}
