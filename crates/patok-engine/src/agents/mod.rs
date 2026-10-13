//! The agents the engine runs, behind one common [`Agent`] interface: the
//! pure per-agent logic of the planner, research, builder, reviewer and
//! discovery sessions -- the role each agent announces, its system and user
//! prompts, its tool allowlist, its session label and the validation turning
//! a session's captured events into accept or reject. The trait and its
//! implementations know nothing about the session machinery that runs them:
//! pure data in and pure results out (T178.1).

mod builder;
mod discovery;
mod planner;
mod research;
mod reviewer;

use patok_core::event::{AgentEvent, AgentRole};
use patok_providers::Tool;

pub(crate) use self::builder::{BuilderAgent, claims_section, trim_claims};
pub(crate) use self::discovery::DiscoveryAgent;
pub(crate) use self::planner::{AppendPlannerAgent, PlanAgent};
pub(crate) use self::research::{ResearchAgent, ResearchQueueAgent};
pub(crate) use self::reviewer::{
    FileReviewerAgent, Findings, ReviewHistory, ReviewOutcome, ReviewerAgent, SkipInputs,
    compute_verdict, is_generated, merge_findings, skip_review,
};

/// Everything a caller needs to drive one agent session, and nothing about
/// how to run it: no engine, no cancellation token, no session machinery --
/// pure data in and pure results out.
pub(crate) trait Agent {
    /// The payload one validated session produces.
    type Session;

    /// The role the agent announces.
    fn role(&self) -> AgentRole;
    /// The system prompt appended to the CLI's own.
    fn system_prompt(&self) -> &'static str;
    /// The user prompt, built from this session's inputs: the task, an
    /// accepted plan, a prior research report or the builder claims, as
    /// applicable.
    fn user_prompt(&self) -> String;
    /// The short transcript label.
    fn label(&self) -> String;
    /// The provider-neutral tool allowlist.
    fn tools(&self) -> &'static [Tool];
    /// Validates the events captured from one session of this agent: the
    /// accepted session payload, or the rejection reason (the plan gate for
    /// the planner, the review verdict and findings parsing for the
    /// reviewer, trivial acceptance for agents with no gate).
    fn validate(&self, events: &[AgentEvent]) -> Result<Self::Session, String>;
}

/// The final text captured from a session's events: the final Result message, or
/// the accumulated Text messages as a fallback when there is no usable result (T10.1).
fn captured_text(events: &[AgentEvent]) -> String {
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

/// Name of the optional spec file in the project root, read by the planner for context.
const SPEC_FILE: &str = "SPEC.md";

/// The task-append guideline bullets shared by the append-tasks planner and
/// the two queue-creation prompts: `split_word` is the planner's "request" or
/// the queue prompts' "work", `nothing` is the queue prompts' honesty bullet
/// (the planner passes `None`), and `finish` is the closing line. The bullets
/// follow the caller's own "Guidelines:" header.
fn append_guidelines(split_word: &str, nothing: Option<&str>, finish: &str) -> String {
    let mut guidelines = format!(
        "- Prefer fewer, larger tasks. Each task should deliver working software that a builder can implement and verify in one session, not scaffolding, stubs or documentation. Only split the {split_word} when the parts are truly independent.\n\
         - Do not modify existing tasks, their text, their order or their checkbox state.\n\
         - Each task is a single line in exactly this format: `- [ ] T<N>.1: <description>`, where <N> is the next unused number after the highest T number in the task list (for example `- [ ] T4.1: Add ...`). Use a new number for each task you append, always with the `.1` suffix, and always the `T` prefix, never an `H` or `D` prefix.\n\
         - Write the description as plain text on one line: no markdown, no backticks, no bold or italics, no links, no nested lists and no line breaks inside a task line.\n\
         - Make the description specific enough to implement without further questions: what to build, where it plugs in, and how to tell it works."
    );
    if let Some(nothing) = nothing {
        guidelines.push_str("\n- ");
        guidelines.push_str(nothing);
    }
    guidelines.push_str("\n\n");
    guidelines.push_str(finish);
    guidelines
}

/// The research report block a builder or plan prompt carries as its prior
/// artifact (T68.1): input to verify, never a plan to copy.
fn research_block(research: Option<&str>) -> String {
    research
        .map(|research| {
            format!(
                "\n\nThe research report for this task (a prior artifact, not a plan):\n\n{research}\n\nUse it as input; verify anything you rely on."
            )
        })
        .unwrap_or_default()
}

/// The task literal the agent prompt tests share.
#[cfg(test)]
use patok_core::task::Task;

#[cfg(test)]
pub(crate) fn test_task() -> Task {
    Task {
        id: "T8.1".into(),
        origin: Some('T'),
        description: "Add the per-task plan stage".into(),
        done: false,
        line: 1,
        raw: "- [ ] T8.1: Add the per-task plan stage".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
