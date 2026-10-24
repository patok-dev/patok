//! The planner agents: the per-task plan agent whose captured plan must pass
//! the plan gate before the engine hands the task to the builder (T10.1),
//! and the append-tasks planner that turns a user request into task lines
//! appended to the project's task list.

use patok_core::event::{AgentEvent, AgentRole};
use patok_core::task::{CHECKED, Task, UNCHECKED};
use patok_providers::{PLAN_TOOLS, PLANNER_TOOLS, Tool};

use super::{Agent, SPEC_FILE, append_guidelines_with, captured_text, research_block};
use crate::TASK_FILE;
use crate::task_list::file::LastGroup;

/// The two sections a plan must have to pass the gate.
const REQUIRED_SECTIONS: [&str; 2] = ["File Operations", "Verification"];

/// Gates a plan captured from a plan session.
///
/// The plan is accepted only when it contains a File Operations section and a
/// Verification section, each with at least one non-empty entry. Headings are matched
/// case-insensitively at any level; entries are the non-blank lines under a heading
/// until the next heading. On rejection the returned reason names every missing or
/// empty section, for the retry prompt (T10.1).
fn gate_plan(plan: &str) -> Result<(), String> {
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

/// The non-blank lines under the first heading named `name` (case-insensitively, any
/// heading level), up to the next heading. `None` when the plan has no such heading.
fn section_entries<'a>(plan: &'a str, name: &str) -> Option<Vec<&'a str>> {
    crate::markdown::section(plan, name).map(|found| {
        plan[found.start..found.end]
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .collect()
    })
}

/// Appended to the CLI's own system prompt for per-task plan sessions (T10.1).
const PLAN_SYSTEM: &str = "You are the Plan agent in an autonomous build loop. \
Before a task is implemented you write one implementation plan for a builder colleague who will carry it out. \
You may only read files; you have no shell. \
Edit no files: return the plan as your final message.";

/// The two sections every plan must end with (T10.1), shared by the plan and
/// plan-retry prompts.
const PLAN_SECTIONS: &str = "The plan must end with these two sections, in this order:\n\n\
## File Operations\n\
Every file to create or change, one per line, with what happens to each and why.\n\n\
## Verification\n\
How to prove the work: the checks or commands to run and the result that means success.";

fn plan(task: &Task, research: Option<&str>) -> String {
    format!(
        "Write one implementation plan for the task below, for a builder colleague to carry out.\n\n\
         Task {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only.{research}\n\n\
         {PLAN_SECTIONS}",
        id = task.id,
        description = task.description,
        research = research_block(research),
    )
}

/// The retry prompt after the plan gate rejected the first plan (T10.1): the new plan
/// must fix the rejection, so it names the reason and carries the rejected plan.
fn plan_retry(task: &Task, rejected: &str, reason: &str, research: Option<&str>) -> String {
    format!(
        "The plan you wrote for the task below was rejected. Write a new implementation plan for a builder colleague to carry out.\n\n\
         Task {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only.{research}\n\n\
         Why the previous plan was rejected:\n{reason}\n\n\
         The rejected plan:\n{rejected}\n\n\
         Write a new plan that fixes the rejection. {PLAN_SECTIONS}",
        id = task.id,
        description = task.description,
        research = research_block(research),
    )
}

/// Appended to the CLI's own system prompt for append-tasks (planner) sessions.
const PLANNER_SYSTEM: &str = "You are the Planner agent in an autonomous build loop. \
You turn a short user request into task lines appended to the project's task list. \
You may only read, edit and write files; you have no shell. \
Never modify, reorder, check off or delete existing tasks or any other existing text: only append new lines at the end of the task list.";

/// The append-tasks planner prompt (T5.6): the agent inspects the last task
/// group itself and decides whether to join it or open a new group.
fn planner(request: &str, last: Option<&LastGroup>) -> String {
    format!(
        "Expand the request below into one or a few comprehensive tasks and append them to {TASK_FILE}.\n\n\
         Request:\n{request}\n\n\
         Current task groups:\n{state}\n\n\
         Steps:\n\
         1. If {SPEC_FILE} exists in the project root, read it for context.\n\
         2. Read the last 20 lines of {TASK_FILE} and inspect the last existing task group yourself: check that its tasks and their checkboxes match the group state above, and work out what concern the group covers.\n\
         3. Decide whether to join that group or open a new one: join it only if it still has pending work and the request belongs to the same concern; otherwise open a new group. Use the IDs given above for your choice.\n\
         4. Append the new task lines at the end of {TASK_FILE}. Do not change anything above them.\n\n\
         Guidelines:\n{guidelines}",
        state = last_group_block(last),
        guidelines = planner_guidelines(),
    )
}

/// The last task group's state and the join-or-open decision rule (T1.1),
/// with the concrete IDs worked out so the planner does not compute them.
fn last_group_block(last: Option<&LastGroup>) -> String {
    let Some(group) = last else {
        return format!("{TASK_FILE} has no T tasks yet: open the first group at T1.1.");
    };
    let (n, m) = (group.number, group.highest_minor);
    let mut block = format!(
        "The last task group in {TASK_FILE} is T{n} ({pending} of {total} tasks pending, highest minor number {m}):\n",
        pending = group.pending,
        total = group.tasks.len(),
    );
    for task in &group.tasks {
        let checkbox = if task.done { CHECKED } else { UNCHECKED };
        block.push_str(&format!("{checkbox}{}: {}\n", task.id, task.description));
    }
    block.push('\n');
    if group.pending > 0 {
        block.push_str(&format!(
            "The group still has pending work (a partly done group counts). \
             If the request belongs to the same concern as this group, join it: your first task is T{n}.{next_minor}. \
             Otherwise open a new group: your first task is T{next}.1.\n\
             Never reuse an older group number or skip a number: every task you append belongs to group T{n} or T{next}.",
            next_minor = m + 1,
            next = n + 1,
        ));
    } else {
        block.push_str(&format!(
            "Every task in this group is done, so never join it: open a new group starting at T{next}.1.\n\
             Never reuse an older group number or skip a number: every task you append belongs to group T{next}.",
            next = n + 1,
        ));
    }
    block
}

/// The append-tasks planner's ID-format bullet (T1.1): a task either joins
/// the last group or opens the next one.
const PLANNER_ID_RULE: &str = "Each task is a single line in exactly one of two formats: `- [ ] T<N>.<M+1>: <description>` to join the last group, or `- [ ] T<N+1>.1: <description>` to open a new group, where <N> is the last group's number and <M> its highest minor number as shown above. Further tasks continue the minor numbers of the same group (for example T<N+1>.2). Always use the `T` prefix, never an `H` or `D` prefix.";

/// The append-tasks planner's guidelines.
fn planner_guidelines() -> String {
    append_guidelines_with(
        "request",
        PLANNER_ID_RULE,
        None,
        "Finish with a one-line summary of the tasks you added, saying whether they joined the last group or opened a new one.",
    )
}

/// The per-task plan agent: its captured plan must pass the plan gate, with one
/// retry that carries the rejected plan and the rejection reason.
pub(crate) struct PlanAgent {
    task: Task,
    research: Option<String>,
    /// The rejected plan and the rejection reason of the first attempt, when
    /// this is the retry.
    rejected: Option<(String, String)>,
}

impl PlanAgent {
    /// The first attempt's agent, with the task and its prior research
    /// report (T68.1).
    pub(crate) fn new(task: &Task, research: Option<&str>) -> Self {
        Self {
            task: task.clone(),
            research: research.map(String::from),
            rejected: None,
        }
    }

    /// The retry's agent: the events of the rejected attempt become the
    /// rejected plan its prompt carries.
    pub(crate) fn retry(&self, events: &[AgentEvent], reason: &str) -> Self {
        Self {
            task: self.task.clone(),
            research: self.research.clone(),
            rejected: Some((captured_text(events), reason.to_string())),
        }
    }
}

impl Agent for PlanAgent {
    type Session = String;

    fn role(&self) -> AgentRole {
        AgentRole::Planner
    }

    fn system_prompt(&self) -> &'static str {
        PLAN_SYSTEM
    }

    fn user_prompt(&self) -> String {
        match &self.rejected {
            Some((rejected, reason)) => {
                plan_retry(&self.task, rejected, reason, self.research.as_deref())
            }
            None => plan(&self.task, self.research.as_deref()),
        }
    }

    fn label(&self) -> String {
        "plan".into()
    }

    fn tools(&self) -> &'static [Tool] {
        PLAN_TOOLS
    }

    fn validate(&self, events: &[AgentEvent]) -> Result<Self::Session, String> {
        let text = captured_text(events);
        gate_plan(&text).map(|()| text)
    }
}

/// The append-tasks planner: its session appends task lines; the file-based
/// `check_appended` validation runs engine-side, so this agent's own gate is
/// trivial acceptance. The prompt carries the task file's last group (T1.1)
/// so the planner can decide between joining it and opening a new one.
pub(crate) struct AppendPlannerAgent {
    request: String,
    last_group: Option<LastGroup>,
}

impl AppendPlannerAgent {
    pub(crate) fn new(request: &str, last_group: Option<LastGroup>) -> Self {
        Self {
            request: request.into(),
            last_group,
        }
    }
}

impl Agent for AppendPlannerAgent {
    type Session = ();

    fn role(&self) -> AgentRole {
        AgentRole::Planner
    }

    fn system_prompt(&self) -> &'static str {
        PLANNER_SYSTEM
    }

    fn user_prompt(&self) -> String {
        planner(&self.request, self.last_group.as_ref())
    }

    fn label(&self) -> String {
        "planner".into()
    }

    fn tools(&self) -> &'static [Tool] {
        PLANNER_TOOLS
    }

    fn validate(&self, _events: &[AgentEvent]) -> Result<Self::Session, String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::test_task;
    use crate::task_list::file::{last_group, validate_append};

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
    fn planner_prompt_carries_request_and_format_rules() {
        let p = planner("add a login page", None);
        assert!(p.contains("add a login page"));
        assert!(p.contains(TASK_FILE) && p.contains(SPEC_FILE));
        assert!(p.contains("last 20 lines"));
        assert!(p.contains("inspect the last existing task group"));
        assert!(p.contains("Decide whether to join"));
        assert!(p.find("inspect the last existing task group") < p.find("Decide whether to join"));
        assert!(p.find("Decide whether to join") < p.find("Append the new task lines"));
        assert!(p.contains("joined the last group or opened a new one"));
        assert!(p.contains("- [ ] T<N>.<M+1>: <description>"));
        assert!(p.contains("- [ ] T<N+1>.1: <description>"));
        assert!(p.contains("T1.1"));
        assert!(p.contains("no markdown"));
        // The prompt is real newlines, not literal escape sequences.
        assert!(!p.contains("\\n"));
    }

    /// T1.1: a partly done last group is shown with its state, and the
    /// planner may join it or open the next group.
    #[test]
    fn planner_prompt_offers_joining_a_partly_done_group() {
        let group = last_group("- [x] T3.1: a\n- [ ] T3.2: b\n").unwrap();
        let p = planner("add a login page", Some(&group));
        assert!(p.contains("- [x] T3.1: a"));
        assert!(p.contains("- [ ] T3.2: b"));
        assert!(p.contains("1 of 2"));
        assert!(p.contains("T3.3"));
        assert!(p.contains("T4.1"));
        assert!(p.contains("same concern"));
        assert!(!p.contains("\\n"));
    }

    /// T1.1: a fully done last group is never joined.
    #[test]
    fn planner_prompt_never_joins_a_fully_done_group() {
        let group = last_group("- [x] T3.1: a\n- [x] T3.2: b\n").unwrap();
        let p = planner("add a login page", Some(&group));
        assert!(p.contains("T4.1"));
        assert!(p.contains("never join"));
        assert!(!p.contains("T3.3"));
    }

    /// The task IDs the prompt offers ("your first task is T3.3.",
    /// "starting at T4.1.", "open the first group at T1.1.").
    fn offered_ids(prompt: &str) -> Vec<String> {
        let mut found = Vec::new();
        for marker in [
            "your first task is ",
            "starting at ",
            "open the first group at ",
        ] {
            for (at, _) in prompt.match_indices(marker) {
                let id: String = prompt[at + marker.len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
                    .collect();
                found.push(id.trim_end_matches('.').to_string());
            }
        }
        found.sort();
        found
    }

    /// T5.6: every ID the prompt offers for joining or opening a group is one
    /// the append validator accepts, and an ID the prompt does not offer
    /// (a skipped group) is rejected.
    #[test]
    fn planner_prompt_offers_only_ids_the_validator_accepts() {
        let accepts =
            |before: &str, id: &str| validate_append(before, &format!("{before}- [ ] {id}: c\n"));

        let partly = "# T\n- [x] T3.1: a\n- [ ] T3.2: b\n";
        let group = last_group(partly).unwrap();
        let ids = offered_ids(&planner("x", Some(&group)));
        assert_eq!(ids, ["T3.3", "T4.1"]);
        for id in &ids {
            assert_eq!(accepts(partly, id), Ok(1), "{id}");
        }
        assert!(!ids.contains(&"T5.1".to_string()));
        let err = accepts(partly, "T5.1").unwrap_err();
        assert!(err.contains("skipped"), "{err}");

        // The validator would also accept T3.3 here (see file.rs T2.1); only
        // the prompt keeps a fully done group closed.
        let done = "# T\n- [x] T3.1: a\n- [x] T3.2: b\n";
        let group = last_group(done).unwrap();
        let ids = offered_ids(&planner("x", Some(&group)));
        assert_eq!(ids, ["T4.1"]);
        assert_eq!(accepts(done, "T4.1"), Ok(1));

        let ids = offered_ids(&planner("x", None));
        assert_eq!(ids, ["T1.1"]);
        assert_eq!(accepts("", "T1.1"), Ok(1));
    }

    #[test]
    fn plan_prompt_carries_task_and_format_rules() {
        let p = plan(&test_task(), None);
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        assert!(p.contains(TASK_FILE));
        assert!(p.contains("## File Operations"));
        assert!(p.contains("## Verification"));
        assert!(p.find("## File Operations") < p.find("## Verification"));
        assert!(p.contains("builder colleague"));
        // The format rules are real newlines, not literal escape sequences.
        assert!(!p.contains("\\n"));
        // Without a research report there is no prior-artifact block.
        assert!(!p.contains("research report"));
    }

    #[test]
    fn the_plan_prompt_carries_the_research_report_as_its_prior_artifact() {
        let report = "1. What does the engine do today?\nIt runs tasks.";
        let p = plan(&test_task(), Some(report));
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        assert!(p.contains("The research report for this task (a prior artifact, not a plan)"));
        assert!(p.contains(report));
        assert!(p.contains("Use it as input; verify anything you rely on."));
        // The report sits before the mandatory-sections rules.
        assert!(p.find(report) < p.find("## File Operations"));
    }

    #[test]
    fn the_retry_prompt_carries_the_rejected_plan_the_reason_and_the_report() {
        let p = plan_retry(
            &test_task(),
            "no sections at all",
            "missing a File Operations section",
            Some("1. What does the engine do today?"),
        );
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        assert!(p.contains("no sections at all"));
        assert!(p.contains("missing a File Operations section"));
        assert!(p.contains("rejected"));
        assert!(p.contains("1. What does the engine do today?"));
        assert!(p.contains("## File Operations"));
        assert!(p.contains("## Verification"));
    }
}
