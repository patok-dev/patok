//! The plan gate: validates the plan text captured from a
//! plan session before the engine hands the task to the builder (T10.1).

use patok_core::event::AgentEvent;

/// The two sections a plan must have to pass the gate.
const REQUIRED_SECTIONS: [&str; 2] = ["File Operations", "Verification"];

/// Gates a plan captured from a plan session.
///
/// The plan is accepted only when it contains a File Operations section and a
/// Verification section, each with at least one non-empty entry. Headings are matched
/// case-insensitively at any level; entries are the non-blank lines under a heading
/// until the next heading. On rejection the returned reason names every missing or
/// empty section, for the retry prompt (T10.1).
pub fn gate_plan(plan: &str) -> Result<(), String> {
    let mut reasons = Vec::new();
    for name in REQUIRED_SECTIONS {
        match section_entries(plan, name) {
            None => reasons.push(format!("missing a {name} section")),
            Some(entries) if entries.is_empty() => {
                reasons.push(format!("the {name} section has no entries"))
            }
            Some(_) => {}
        }
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons.join("; "))
    }
}

/// The plan text captured from a plan session's events: the final Result message, or
/// the accumulated Text messages as a fallback when there is no usable result (T10.1).
pub fn captured_text(events: &[AgentEvent]) -> String {
    let result = events.iter().rev().find_map(|event| match event {
        AgentEvent::Result { text } if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    });
    result.unwrap_or_else(|| {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// The non-blank lines under the first heading named `name` (case-insensitively, any
/// heading level), up to the next heading. `None` when the plan has no such heading.
fn section_entries<'a>(plan: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let mut in_section = false;
    let mut entries = Vec::new();
    for line in plan.lines() {
        if let Some(heading) = heading_name(line) {
            if in_section {
                break;
            }
            in_section = heading.eq_ignore_ascii_case(name);
        } else if in_section {
            let entry = line.trim();
            if !entry.is_empty() {
                entries.push(entry);
            }
        }
    }
    if in_section { Some(entries) } else { None }
}

/// The name of a markdown heading line (`#` through `######`), or `None` for any other
/// line. The name is the heading text with the hashes and surrounding space stripped.
fn heading_name(line: &str) -> Option<&str> {
    let after_hashes = line.trim_start().strip_prefix('#')?.trim_start_matches('#');
    let name = after_hashes.trim();
    if name.is_empty() { None } else { Some(name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_plan_is_accepted() {
        let plan = "\
Implementation steps first.

## File Operations
Create src/plan.rs with the gate function.
Update src/lib.rs to declare the module.

## Verification
cargo test -p patok-engine passes with the new gate tests.";
        assert_eq!(gate_plan(plan), Ok(()));
    }

    #[test]
    fn headings_match_case_insensitively_at_any_level() {
        let plan = "## file operations\n- edit engine.rs\n#### VERIFICATION\nrun cargo test";
        assert_eq!(gate_plan(plan), Ok(()));
    }

    #[test]
    fn a_missing_file_operations_section_is_rejected() {
        let plan = "\
Implementation steps first.

## Verification
cargo test -p patok-engine passes.";
        let reason = gate_plan(plan).unwrap_err();
        assert!(
            reason.contains("File Operations"),
            "reason should name File Operations: {reason}"
        );
        assert!(!reason.contains("Verification"), "reason: {reason}");
    }

    #[test]
    fn a_missing_verification_section_is_rejected() {
        let plan = "\
Implementation steps first.

## File Operations
Create src/plan.rs with the gate function.";
        let reason = gate_plan(plan).unwrap_err();
        assert!(
            reason.contains("Verification"),
            "reason should name Verification: {reason}"
        );
        assert!(!reason.contains("File Operations"), "reason: {reason}");
    }

    #[test]
    fn empty_sections_are_rejected() {
        // Blank lines under a heading do not count as entries; prose after the last
        // heading would, so this plan ends at the Verification heading.
        let both_empty = "## File Operations\n\n## Verification\n";
        let reason = gate_plan(both_empty).unwrap_err();
        assert!(reason.contains("File Operations"));
        assert!(reason.contains("Verification"));

        let blank_entries = "## File Operations\n   \n\t\n## Verification\n \n \n";
        assert!(gate_plan(blank_entries).is_err());

        let only_verification_filled = "## File Operations\n## Verification\nrun cargo test";
        let reason = gate_plan(only_verification_filled).unwrap_err();
        assert!(reason.contains("File Operations"));
        assert!(!reason.contains("Verification"), "reason: {reason}");

        let only_file_operations_filled = "## File Operations\nedit engine.rs\n## Verification\n";
        let reason = gate_plan(only_file_operations_filled).unwrap_err();
        assert!(reason.contains("Verification"));
        assert!(!reason.contains("File Operations"), "reason: {reason}");
    }

    #[test]
    fn the_sections_may_come_in_either_order() {
        let plan = "\
## Verification
cargo test -p patok-engine passes.

## File Operations
Create src/plan.rs with the gate function.
Update src/lib.rs to declare the module.";
        assert_eq!(gate_plan(plan), Ok(()));
    }

    #[test]
    fn entries_stop_at_the_next_heading() {
        // The File Operations section ends at the Verification heading, so it is not
        // accepted on the strength of the Verification entries alone.
        let plan = "## File Operations\n## Verification\ncargo test passes";
        let reason = gate_plan(plan).unwrap_err();
        assert!(reason.contains("File Operations"), "reason: {reason}");
    }

    #[test]
    fn a_plan_without_any_headings_is_rejected_with_both_reasons() {
        let reason = gate_plan("Just some implementation prose, no sections.").unwrap_err();
        assert!(reason.contains("File Operations"));
        assert!(reason.contains("Verification"));
    }

    #[test]
    fn the_result_event_is_the_plan() {
        let events = [
            AgentEvent::Text {
                text: "thinking".into(),
            },
            AgentEvent::Result {
                text: "## File Operations\nedit\n\n## Verification\ntest".into(),
            },
        ];
        assert_eq!(
            captured_text(&events),
            "## File Operations\nedit\n\n## Verification\ntest"
        );
    }

    #[test]
    fn the_text_events_accumulate_as_the_fallback() {
        let events = [
            AgentEvent::Text {
                text: "## File Operations\nedit engine.rs".into(),
            },
            AgentEvent::Text {
                text: "## Verification\nrun cargo test".into(),
            },
        ];
        assert_eq!(
            captured_text(&events),
            "## File Operations\nedit engine.rs\n## Verification\nrun cargo test"
        );
        // An empty result falls back to the text events too.
        let with_empty_result = [
            AgentEvent::Text {
                text: "## File Operations\nedit".into(),
            },
            AgentEvent::Result { text: "  ".into() },
        ];
        assert_eq!(
            captured_text(&with_empty_result),
            "## File Operations\nedit"
        );
    }
}
