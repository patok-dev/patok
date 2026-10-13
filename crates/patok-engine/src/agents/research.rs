//! The research agents: the per-task research agent that investigates a
//! task before it is planned (T68.1), and the two queue-creation runs
//! (T69.1), the Research agent's second duty of creating the project's
//! initial task queue.

use patok_core::event::{AgentEvent, AgentRole};
use patok_core::task::Task;
use patok_providers::{RESEARCH_TOOLS, Tool};

use super::{Agent, SPEC_FILE, append_guidelines, captured_text};
use crate::TASK_FILE;

/// Appended to the CLI's own system prompt for per-task research sessions (T68.1).
const RESEARCH_SYSTEM: &str = "You are the Research agent in an autonomous build loop. \
You investigate a task before it is planned: you may read files, search the project and run \
read-only shell commands, but you must not edit any project files. \
Return your research report as your final message, opening with the investigation questions \
you pursued and then the answers to them.";

fn research(task: &Task) -> String {
    format!(
        "Investigate the task below and return a research report for the planner who will plan it.\n\n\
         Task {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only.\n\n\
         Your report, returned as your final message:\n\
         1. Open with a numbered list of 3 to 5 investigation questions you pursued -- \
questions about what the codebase currently does (existing conventions, the files and modules \
the task touches, test and build conventions), not about how to implement the task, and each \
answerable from this project alone.\n\
         2. Answer every question with what exists today, citing the file paths and lines your \
answer rests on. Answer \"not found\" when the project has nothing to show; never speculate.\n\
         3. End with a brief additional-findings section: anything worth knowing that no \
question captured.\n\n\
         Report what exists, not what should exist. The report is your final message.",
        id = task.id,
        description = task.description,
    )
}

/// Appended to the CLI's own system prompt for the research queue-creation runs (T69.1):
/// the Research agent's second duty, creating the project's initial task queue.
const RESEARCH_QUEUE_SYSTEM: &str = "You are the Research agent in an autonomous build loop. \
Your two duties are investigating a task before it is planned and creating the project's \
initial task queue. In this run you investigate the project and create the queue: \
you may read files, search the project and run read-only shell commands, and you may \
only append new task lines at the end of the task list. \
Never modify, reorder, check off or delete existing tasks or any other existing text: \
only append new lines at the end of the task list. \
Return your research report as your final message, opening with the investigation \
questions you pursued and then the answers to them.";

/// The two queue-creation prompts' guidelines: the same rules plus the
/// honesty bullet and the research-report closing line.
fn queue_guidelines() -> String {
    append_guidelines(
        "work",
        Some("If you find nothing credible to add, add no task and say so."),
        "Finish with your research report as your final message, opening with the investigation questions you pursued and then the answers to them.",
    )
}

/// The NeedsQueue scenario's empty submit (T69.1): the Research agent reads the
/// spec and creates the project's initial task queue.
fn research_queue_bootstrap() -> String {
    format!(
        "Investigate the project and create its initial task queue, appending the tasks to {TASK_FILE}.\n\n\
         Steps:\n\
         1. If {SPEC_FILE} exists in the project root, read it first: it is the plan, and the tasks you create implement what it describes.\n\
         2. Read the last 20 lines of {TASK_FILE} to find the highest task number in use, so you can pick the next number.\n\
         3. Investigate the project: detect the tech stack, read the source and note how the spec's work splits into independently verifiable tasks.\n\
         4. Append the new task lines at the end of {TASK_FILE}. Do not change anything above them.\n\n\
         Guidelines:\n\n{guidelines}",
        guidelines = queue_guidelines(),
    )
}

/// The QueueComplete scenario's empty submit (T69.1): the Research agent scans
/// for gaps and worthwhile follow-up work and appends it as tasks.
fn research_queue_scan() -> String {
    format!(
        "Investigate the project for gaps and worthwhile follow-up work, and append it as tasks to {TASK_FILE}.\n\n\
         Steps:\n\
         1. If {SPEC_FILE} exists in the project root, read it for context.\n\
         2. Read the last 20 lines of {TASK_FILE} to find the highest task number in use, so you can pick the next number.\n\
         3. Investigate the project for genuinely worthwhile follow-up work: bugs, missing pieces and rough edges left by the build work, not padding or nice-to-haves.\n\
         4. Append the new task lines at the end of {TASK_FILE}. Do not change anything above them.\n\n\
         Guidelines:\n\n{guidelines}",
        guidelines = queue_guidelines(),
    )
}

/// The per-task research agent: its session's final message is the research
/// report the planner receives as prior art (T68.1). The empty-report
/// warning stays in the stage, so this agent's own gate is trivial
/// acceptance.
pub(crate) struct ResearchAgent {
    task: Task,
}

impl ResearchAgent {
    pub(crate) fn new(task: &Task) -> Self {
        Self { task: task.clone() }
    }
}

impl Agent for ResearchAgent {
    type Session = String;

    fn role(&self) -> AgentRole {
        AgentRole::Research
    }

    fn system_prompt(&self) -> &'static str {
        RESEARCH_SYSTEM
    }

    fn user_prompt(&self) -> String {
        research(&self.task)
    }

    fn label(&self) -> String {
        "research".into()
    }

    fn tools(&self) -> &'static [Tool] {
        RESEARCH_TOOLS
    }

    fn validate(&self, events: &[AgentEvent]) -> Result<Self::Session, String> {
        Ok(captured_text(events))
    }
}

/// One of the two queue-creation runs (T69.1): the Research agent investigates
/// the project and appends the resulting tasks. The appended tasks pass the
/// file-based append validation engine-side; the session's own payload is the
/// research report saved on a pass.
pub(crate) struct ResearchQueueAgent {
    bootstrap: bool,
}

impl ResearchQueueAgent {
    /// The NeedsQueue scenario's queue-creation run.
    pub(crate) fn bootstrap() -> Self {
        Self { bootstrap: true }
    }

    /// The QueueComplete scenario's queue-creation run.
    pub(crate) fn scan() -> Self {
        Self { bootstrap: false }
    }
}

impl Agent for ResearchQueueAgent {
    type Session = String;

    fn role(&self) -> AgentRole {
        AgentRole::Research
    }

    fn system_prompt(&self) -> &'static str {
        RESEARCH_QUEUE_SYSTEM
    }

    fn user_prompt(&self) -> String {
        if self.bootstrap {
            research_queue_bootstrap()
        } else {
            research_queue_scan()
        }
    }

    fn label(&self) -> String {
        "research".into()
    }

    fn tools(&self) -> &'static [Tool] {
        RESEARCH_TOOLS
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
    fn the_research_system_prompt_states_the_stage_rules() {
        assert!(RESEARCH_SYSTEM.contains("You are the Research agent"));
        // It investigates before the task is planned.
        assert!(RESEARCH_SYSTEM.contains("before it is planned"));
        // It may read, search and run read-only shell commands...
        assert!(RESEARCH_SYSTEM.contains("read files"));
        assert!(RESEARCH_SYSTEM.contains("search the project"));
        assert!(RESEARCH_SYSTEM.contains("read-only shell commands"));
        // ...but must not edit any project files.
        assert!(RESEARCH_SYSTEM.contains("must not edit any project files"));
        // The report is the final message, questions first then answers.
        assert!(RESEARCH_SYSTEM.contains("final message"));
        assert!(RESEARCH_SYSTEM.contains("investigation questions"));
        assert!(RESEARCH_SYSTEM.contains("then the answers"));
    }

    #[test]
    fn the_research_prompt_carries_the_task_and_the_report_rules() {
        let p = research(&test_task());
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        assert!(p.contains(TASK_FILE));
        assert!(p.contains("treat it as read-only"));
        assert!(p.contains("3 to 5 investigation questions"));
        assert!(p.contains("what the codebase currently does"));
        assert!(p.contains("The report is your final message"));
        // The format rules are real newlines, not literal escape sequences.
        assert!(!p.contains("\\n"));
    }

    #[test]
    fn the_research_queue_system_prompt_states_the_two_duties_and_the_rules() {
        assert!(RESEARCH_QUEUE_SYSTEM.contains("You are the Research agent"));
        // The two duties: investigating a task, and creating the initial queue.
        assert!(RESEARCH_QUEUE_SYSTEM.contains("investigating a task before it is planned"));
        assert!(RESEARCH_QUEUE_SYSTEM.contains("creating the project's initial task queue"));
        // It may read, search and run read-only shell commands...
        assert!(RESEARCH_QUEUE_SYSTEM.contains("read files"));
        assert!(RESEARCH_QUEUE_SYSTEM.contains("search the project"));
        assert!(RESEARCH_QUEUE_SYSTEM.contains("read-only shell commands"));
        // ...but may only append new task lines, never modify existing text.
        assert!(RESEARCH_QUEUE_SYSTEM.contains("only append new task lines"));
        assert!(RESEARCH_QUEUE_SYSTEM.contains("Never modify, reorder, check off or delete"));
        // The report is the final message, questions first then answers.
        assert!(RESEARCH_QUEUE_SYSTEM.contains("final message"));
        assert!(RESEARCH_QUEUE_SYSTEM.contains("investigation questions"));
        assert!(RESEARCH_QUEUE_SYSTEM.contains("then the answers"));
    }

    #[test]
    fn the_queue_creation_prompts_carry_the_format_rules() {
        for p in [research_queue_bootstrap(), research_queue_scan()] {
            assert!(p.contains(TASK_FILE) && p.contains(SPEC_FILE));
            assert!(p.contains("last 20 lines"));
            assert!(p.contains("- [ ] T<N>.1: <description>"));
            assert!(p.contains("no markdown"));
            assert!(p.contains("Do not modify existing tasks"));
            assert!(p.contains("research report as your final message"));
            // The format rules are real newlines, not literal escape sequences.
            assert!(!p.contains("\\n"));
        }
        // Each prompt names its own run.
        assert!(research_queue_bootstrap().contains("initial task queue"));
        assert!(research_queue_scan().contains("gaps and worthwhile follow-up work"));
    }
}
