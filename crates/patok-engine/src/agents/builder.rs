//! The builder agent: the per-task build session's prompt, its build-claims
//! artifact shaping (the claims-section extraction and the
/// verification-results trimming) and the agent itself.
use patok_core::event::{AgentEvent, AgentRole};
use patok_core::task::Task;
use patok_providers::{BUILDER_TOOLS, Tool};

use super::{Agent, captured_text, research_block};
use crate::TASK_FILE;

/// Appended to the CLI's own system prompt.
const SYSTEM: &str = "You are the Builder agent in an autonomous build loop. \
Implement exactly the task you are given, no unrequested extras. \
Do not run git commit or any other git command that changes repository state: the loop commits your work. \
Do not edit the task list file: the loop marks tasks done. \
Run the project's build and tests where they exist and fix failures before finishing. \
End with a brief summary of what you changed.";

/// The build claims section the builder's final message must end with: the artifact the review
/// stage reviews, written for a reviewer with a fresh context.
const BUILD_CLAIMS: &str = "End your final message with a section titled `## Build Claims`, and nothing after it. \
It is the artifact a reviewer with a fresh context reads: someone who sees only that section and the code, so it must be precise and honest. \
It contains these sub-sections, in this order:

## Files Changed
Every file you created or modified, one line each, marked CREATE or MODIFY plus one line saying what changed.

## Verification Results
Every check you ran with the exact command and its result: PASS, FAIL or SKIPPED.

## Claims
A checkbox list of specific, verifiable statements about what the work does (`- [ ]` lines the reviewer can check against the code).

## Wire-Up Evidence
For every new function, field or config field, the exact production call site as `file:line` and its caller, or `N/A: no new public surface`.

## Gaps and Assumptions
The uncertainties, untested edge cases and deviations from the plan or task.";

fn builder(task: &Task, plan: Option<&str>, research: Option<&str>) -> String {
    let plan = plan
        .map(|plan| {
            format!(
                "\n\nOne implementation plan was accepted for this task; carry it out:\n\n{plan}"
            )
        })
        .unwrap_or_default();
    // Without a plan the research report is the builder's only prior artifact,
    // so it travels along (T68.1); with a plan the planner already read it.
    let research = if plan.is_empty() {
        research_block(research)
    } else {
        String::new()
    };
    format!(
        "Implement this task.\n\nTask {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only.{plan}{research}\n\n\
         {BUILD_CLAIMS}",
        id = task.id,
        description = task.description,
    )
}

/// Findings above this many lines in the verification-results section of the
/// claims trigger the trim.
const VERIFICATION_TRIM_LINES: usize = 100;
/// The head and tail the trimmed verification section keeps.
const TRIM_HEAD: usize = 20;
const TRIM_TAIL: usize = 10;
/// Substrings that keep a trimmed middle line in the section.
const TRIM_KEEP: [&str; 4] = ["failed", "error", "panic", "assert"];

/// The claims file's build-claims section, from its heading to the end of the
/// text (`None` when the builder's final message has no such heading). The
/// heading matches case-insensitively at any level.
pub(crate) fn claims_section(text: &str) -> Option<&str> {
    crate::markdown::section(text, "Build Claims").map(|found| &text[found.start..])
}

/// Trims an over-long verification-results section of the build claims: a section over 100 lines is reduced to its first 20
/// lines, a "trimmed N lines" marker, any middle lines mentioning failed,
/// error, panic or assert (case-insensitively), and its last 10 lines.
/// Everything else in the claims is untouched.
pub(crate) fn trim_claims(claims: &str) -> String {
    let Some(bounds) = crate::markdown::section(claims, "Verification Results") else {
        return claims.to_string();
    };
    // The section's content: every line after its heading, up to the next one.
    let content: Vec<&str> = claims[bounds.start..bounds.end].lines().skip(1).collect();
    if content.len() <= VERIFICATION_TRIM_LINES {
        return claims.to_string();
    }
    let heading_end = bounds.start + bounds.heading.len() + 1;
    let mut trimmed = String::with_capacity(claims.len());
    trimmed.push_str(&claims[..heading_end]);
    for line in &content[..TRIM_HEAD] {
        trimmed.push_str(line);
        trimmed.push('\n');
    }
    let middle = &content[TRIM_HEAD..content.len() - TRIM_TAIL];
    let kept: Vec<&&str> = middle
        .iter()
        .filter(|line| {
            let lower = line.to_lowercase();
            TRIM_KEEP.iter().any(|needle| lower.contains(needle))
        })
        .collect();
    trimmed.push_str(&format!(
        "[trimmed {} lines of verification output]\n",
        middle.len() - kept.len()
    ));
    for line in kept {
        trimmed.push_str(line);
        trimmed.push('\n');
    }
    for line in &content[content.len() - TRIM_TAIL..] {
        trimmed.push_str(line);
        trimmed.push('\n');
    }
    trimmed.push_str(&claims[bounds.end..]);
    trimmed
}

/// The per-task builder agent: its session implements the task, carrying the
/// accepted plan (or the research report when there is no plan) as prior
/// art. The session's final message is the payload; its build-claims section
/// becomes the review stage's claims artifact.
pub(crate) struct BuilderAgent {
    task: Task,
    plan: Option<String>,
    research: Option<String>,
}

impl BuilderAgent {
    pub(crate) fn new(task: &Task, plan: Option<&str>, research: Option<&str>) -> Self {
        Self {
            task: task.clone(),
            plan: plan.map(String::from),
            research: research.map(String::from),
        }
    }
}

impl Agent for BuilderAgent {
    type Session = String;

    fn role(&self) -> AgentRole {
        AgentRole::Builder
    }

    fn system_prompt(&self) -> &'static str {
        SYSTEM
    }

    fn user_prompt(&self) -> String {
        builder(&self.task, self.plan.as_deref(), self.research.as_deref())
    }

    fn label(&self) -> String {
        self.task.id.clone()
    }

    fn tools(&self) -> &'static [Tool] {
        BUILDER_TOOLS
    }

    fn validate(&self, events: &[AgentEvent]) -> Result<Self::Session, String> {
        Ok(captured_text(events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::test_task;

    #[test]
    fn the_builder_prompt_carries_the_accepted_plan() {
        let with = builder(
            &test_task(),
            Some("## File Operations\nedit\n\n## Verification\ntest"),
            // A report exists, but the plan supersedes it: the builder prompt
            // does not repeat it.
            Some("1. What does the engine do today?"),
        );
        assert!(with.contains("Task T8.1: Add the per-task plan stage"));
        assert!(with.contains(TASK_FILE));
        assert!(with.contains("carry it out"));
        assert!(with.contains("## File Operations\nedit\n\n## Verification\ntest"));
        // A plan present means the planner already read the report: the builder
        // prompt does not repeat it.
        assert!(!with.contains("research report"));

        let without = builder(&test_task(), None, None);
        assert!(without.contains("Task T8.1: Add the per-task plan stage"));
        assert!(!without.contains("carry it out"));
        assert!(!without.contains("implementation plan"));
        assert!(!without.contains("research report"));
    }

    #[test]
    fn the_builder_prompt_requires_the_build_claims_section() {
        for p in [
            builder(&test_task(), None, None),
            builder(
                &test_task(),
                Some("## File Operations\nedit\n\n## Verification\ntest"),
                None,
            ),
        ] {
            assert!(p.contains("## Build Claims"));
            assert!(p.contains("## Files Changed"));
            assert!(p.contains("## Verification Results"));
            assert!(p.contains("PASS, FAIL or SKIPPED"));
            assert!(p.contains("## Claims"));
            assert!(p.contains("checkbox list of specific, verifiable statements"));
            assert!(p.contains("## Wire-Up Evidence"));
            assert!(p.contains("N/A: no new public surface"));
            assert!(p.contains("## Gaps and Assumptions"));
            assert!(p.contains("precise and honest"));
            // The claims section is the last thing in the prompt.
            assert!(p.find("## Build Claims").unwrap() > p.find("Implement this task").unwrap());
        }
    }

    #[test]
    fn the_builder_prompt_carries_the_research_report_only_without_a_plan() {
        let report = "1. What does the engine do today?\nIt runs tasks.";
        let p = builder(&test_task(), None, Some(report));
        assert!(p.contains("The research report for this task (a prior artifact, not a plan)"));
        assert!(p.contains(report));
        assert!(!p.contains("carry it out"));
    }

    #[test]
    fn claims_section_starts_at_the_heading() {
        let text = "Summary first.\n\n## Build Claims\n## Files Changed\nCREATE a\n";
        assert_eq!(
            claims_section(text),
            Some("## Build Claims\n## Files Changed\nCREATE a\n")
        );
        assert_eq!(claims_section("no headings"), None);
        // Case-insensitive at any level.
        assert!(claims_section("#### build claims\nx").is_some());
    }

    #[test]
    fn a_short_verification_section_is_kept_whole() {
        let claims = "## Verification Results\n1. cargo test\n2. cargo clippy\n";
        assert_eq!(trim_claims(claims), claims);
        // A claims without the section is untouched.
        assert_eq!(
            trim_claims("## Files Changed\nCREATE a\n"),
            "## Files Changed\nCREATE a\n"
        );
    }

    #[test]
    fn an_over_long_verification_section_is_trimmed() {
        let mut section = String::from("## Verification Results\n");
        for i in 0..120 {
            section.push_str(&format!("line {i} of the output\n"));
        }
        section.push_str("## Claims\n- [ ] it works\n");
        let trimmed = trim_claims(&section);
        // The head and tail survive.
        assert!(trimmed.contains("line 0 of the output"));
        assert!(trimmed.contains("line 19 of the output"));
        assert!(trimmed.contains("line 119 of the output"));
        assert!(trimmed.contains("line 110 of the output"));
        // The middle is gone except the marker and the kept lines.
        assert!(trimmed.contains("[trimmed 90 lines of verification output]"));
        assert!(!trimmed.contains("line 50 of the output"));
        // The sections around the trimmed one are untouched.
        assert!(trimmed.contains("## Claims\n- [ ] it works"));
        assert!(trimmed.starts_with("## Verification Results\n"));
        assert_eq!(trimmed.matches("## Verification Results").count(), 1);
    }

    #[test]
    fn middle_lines_mentioning_failures_survive_the_trim() {
        let mut section = String::from("## Verification Results\n");
        for i in 0..110 {
            if i == 50 {
                section.push_str("error: assertion failed in the middle\n");
            } else {
                section.push_str(&format!("line {i}\n"));
            }
        }
        let trimmed = trim_claims(&section);
        assert!(trimmed.contains("error: assertion failed in the middle"));
        assert!(trimmed.contains("[trimmed 79 lines of verification output]"));
    }
}
