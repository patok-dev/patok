//! The discovery agent: the session that scans the project for worthwhile
//! follow-up work after a build session and appends it to the project's
//! task list. Its result is validated by the file-based discovery-append
//! check engine-side, so its own gate is trivial acceptance.

use patok_core::event::{AgentEvent, AgentRole};
use patok_providers::{DISCOVERY_TOOLS, Tool};

use super::{Agent, SPEC_FILE};
use crate::TASK_FILE;

/// Appended to the CLI's own system prompt for discovery sessions.
const DISCOVERY_SYSTEM: &str = "You are the Discovery agent in an autonomous build loop. \
After a build session you scan the project for worthwhile follow-up work and append it to the project's task list. \
You may only read files; you have no shell and cannot edit or write. \
Never modify, reorder, check off or delete existing tasks or any other existing text: only append new lines at the end of the task list.";

fn discovery() -> String {
    format!(
        "Scan the project for worthwhile follow-up work and append one or a few tasks to {TASK_FILE}.\n\n\
         Steps:\n\
         1. If {SPEC_FILE} exists in the project root, read it for context.\n\
         2. Read the last 20 lines of {TASK_FILE} to find the highest D number in use, so you can pick the next number.\n\
         3. Scan the project for follow-up work that is genuinely worth doing: bugs, missing pieces and rough edges left by the build work, not padding or nice-to-haves.\n\
         4. Append the new task lines at the end of {TASK_FILE}. Do not change anything above them.\n\n\
         Guidelines:\n\
         - Prefer one or a few substantial tasks. Each task should deliver working software that a builder can implement and verify in one session, not scaffolding, stubs or documentation.\n\
         - Do not modify existing tasks, their text, their order or their checkbox state.\n\
         - Each task is a single line in exactly this format: `- [ ] D<N>.1: <description>`, where <N> is the next unused number after the highest D number in the task list (for example `- [ ] D4.1: Add ...`). Use a new number for each task you append, always with the `.1` suffix, and always the `D` prefix, never a `T` or `H` prefix.\n\
         - Write the description as plain text on one line: no markdown, no backticks, no bold or italics, no links, no nested lists and no line breaks inside a task line.\n\
         - Make the description specific enough to implement without further questions: what to build, where it plugs in, and how to tell it works.\n\
         - If you find nothing worth doing, add no task.\n\n\
         Finish with a one-line summary of the tasks you added.",
    )
}

/// One discovery round's agent.
pub(crate) struct DiscoveryAgent;

impl DiscoveryAgent {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Agent for DiscoveryAgent {
    type Session = ();

    fn role(&self) -> AgentRole {
        AgentRole::Discovery
    }

    fn system_prompt(&self) -> &'static str {
        DISCOVERY_SYSTEM
    }

    fn user_prompt(&self) -> String {
        discovery()
    }

    fn label(&self) -> String {
        "discovery".into()
    }

    fn tools(&self) -> &'static [Tool] {
        DISCOVERY_TOOLS
    }

    fn validate(&self, _events: &[AgentEvent]) -> Result<Self::Session, String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_prompt_carries_format_rules() {
        let p = discovery();
        assert!(p.contains(TASK_FILE) && p.contains(SPEC_FILE));
        assert!(p.contains("highest D number"));
        assert!(p.contains("- [ ] D<N>.1: <description>"));
        assert!(p.contains("no markdown"));
        assert!(p.contains("Do not modify existing tasks"));
    }
}
