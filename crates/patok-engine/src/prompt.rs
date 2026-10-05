//! Builder prompts.

use patok_core::task::Task;

use crate::TASK_FILE;

/// Appended to the CLI's own system prompt.
pub const SYSTEM: &str = "You are the Builder agent in an autonomous build loop. \
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

pub fn builder(task: &Task, plan: Option<&str>, research: Option<&str>) -> String {
    let plan = plan
        .map(|plan| {
            format!(
                "\n\nOne implementation plan was accepted for this task; carry it out:\n\n{plan}"
            )
        })
        .unwrap_or_default();
    // Without a plan the research report is the builder's only prior artifact,
    // so it travels along (T68.1); with a plan the planner already read it.
    let research = match (plan.is_empty(), research) {
        (true, Some(research)) => format!(
            "\n\nThe research report for this task (a prior artifact, not a plan):\n\n{research}\n\nUse it as input; verify anything you rely on."
        ),
        _ => String::new(),
    };
    format!(
        "Implement this task.\n\nTask {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only.{plan}{research}\n\n\
         {BUILD_CLAIMS}",
        id = task.id,
        description = task.description,
    )
}

/// Name of the optional spec file in the project root, read by the planner for context.
pub const SPEC_FILE: &str = "SPEC.md";

/// Appended to the CLI's own system prompt for append-tasks (planner) sessions.
pub const PLANNER_SYSTEM: &str = "You are the Planner agent in an autonomous build loop. \
You turn a short user request into task lines appended to the project's task list. \
You may only read, edit and write files; you have no shell. \
Never modify, reorder, check off or delete existing tasks or any other existing text: only append new lines at the end of the task list.";

pub fn planner(request: &str) -> String {
    format!(
        "Expand the request below into one or a few comprehensive tasks and append them to {TASK_FILE}.\n\n\
         Request:\n{request}\n\n\
         Steps:\n\
         1. If {SPEC_FILE} exists in the project root, read it for context.\n\
         2. Read the last 20 lines of {TASK_FILE} to find the highest task number in use, so you can pick the next number.\n\
         3. Append the new task lines at the end of {TASK_FILE}. Do not change anything above them.\n\n\
         Guidelines:\n\
         - Prefer fewer, larger tasks. Each task should deliver working software that a builder can implement and verify in one session, not scaffolding, stubs or documentation. Only split the request when the parts are truly independent.\n\
         - Do not modify existing tasks, their text, their order or their checkbox state.\n\
         - Each task is a single line in exactly this format: `- [ ] T<N>.1: <description>`, where <N> is the next unused number after the highest T number in the task list (for example `- [ ] T4.1: Add ...`). Use a new number for each task you append, always with the `.1` suffix, and always the `T` prefix, never an `H` or `D` prefix.\n\
         - Write the description as plain text on one line: no markdown, no backticks, no bold or italics, no links, no nested lists and no line breaks inside a task line.\n\
         - Make the description specific enough to implement without further questions: what to build, where it plugs in, and how to tell it works.\n\n\
         Finish with a one-line summary of the tasks you added.",
    )
}

/// Appended to the CLI's own system prompt for discovery sessions.
pub const DISCOVERY_SYSTEM: &str = "You are the Discovery agent in an autonomous build loop. \
After a build session you scan the project for worthwhile follow-up work and append it to the project's task list. \
You may only read files; you have no shell and cannot edit or write. \
Never modify, reorder, check off or delete existing tasks or any other existing text: only append new lines at the end of the task list.";

pub fn discovery() -> String {
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

/// Appended to the CLI's own system prompt for per-task research sessions (T68.1).
pub const RESEARCH_SYSTEM: &str = "You are the Research agent in an autonomous build loop. \
You investigate a task before it is planned: you may read files, search the project and run \
read-only shell commands, but you must not edit any project files. \
Return your research report as your final message, opening with the investigation questions \
you pursued and then the answers to them.";

pub fn research(task: &Task) -> String {
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
pub const RESEARCH_QUEUE_SYSTEM: &str = "You are the Research agent in an autonomous build loop. \
Your two duties are investigating a task before it is planned and creating the project's \
initial task queue. In this run you investigate the project and create the queue: \
you may read files, search the project and run read-only shell commands, and you may \
only append new task lines at the end of the task list. \
Never modify, reorder, check off or delete existing tasks or any other existing text: \
only append new lines at the end of the task list. \
Return your research report as your final message, opening with the investigation \
questions you pursued and then the answers to them.";

/// The strict append rules shared by the two queue-creation prompts: the same
/// line format the append-tasks planner is held to.
const QUEUE_GUIDELINES: &str = "Guidelines:\n\n\
         - Prefer fewer, larger tasks. Each task should deliver working software that a builder can implement and verify in one session, not scaffolding, stubs or documentation. Only split the work when the parts are truly independent.\n\
         - Do not modify existing tasks, their text, their order or their checkbox state.\n\
         - Each task is a single line in exactly this format: `- [ ] T<N>.1: <description>`, where <N> is the next unused number after the highest T number in the task list (for example `- [ ] T4.1: Add ...`). Use a new number for each task you append, always with the `.1` suffix, and always the `T` prefix, never an `H` or `D` prefix.\n\
         - Write the description as plain text on one line: no markdown, no backticks, no bold or italics, no links, no nested lists and no line breaks inside a task line.\n\
         - Make the description specific enough to implement without further questions: what to build, where it plugs in, and how to tell it works.\n\
         - If you find nothing credible to add, add no task and say so.\n\n\
         Finish with your research report as your final message, opening with the investigation questions you pursued and then the answers to them.";

/// The NeedsQueue scenario's empty submit (T69.1): the Research agent reads the
/// spec and creates the project's initial task queue.
pub fn research_queue_bootstrap() -> String {
    format!(
        "Investigate the project and create its initial task queue, appending the tasks to {TASK_FILE}.\n\n\
         Steps:\n\
         1. If {SPEC_FILE} exists in the project root, read it first: it is the plan, and the tasks you create implement what it describes.\n\
         2. Read the last 20 lines of {TASK_FILE} to find the highest task number in use, so you can pick the next number.\n\
         3. Investigate the project: detect the tech stack, read the source and note how the spec's work splits into independently verifiable tasks.\n\
         4. Append the new task lines at the end of {TASK_FILE}. Do not change anything above them.\n\n\
         {QUEUE_GUIDELINES}",
    )
}

/// The QueueComplete scenario's empty submit (T69.1): the Research agent scans
/// for gaps and worthwhile follow-up work and appends it as tasks.
pub fn research_queue_scan() -> String {
    format!(
        "Investigate the project for gaps and worthwhile follow-up work, and append it as tasks to {TASK_FILE}.\n\n\
         Steps:\n\
         1. If {SPEC_FILE} exists in the project root, read it for context.\n\
         2. Read the last 20 lines of {TASK_FILE} to find the highest task number in use, so you can pick the next number.\n\
         3. Investigate the project for genuinely worthwhile follow-up work: bugs, missing pieces and rough edges left by the build work, not padding or nice-to-haves.\n\
         4. Append the new task lines at the end of {TASK_FILE}. Do not change anything above them.\n\n\
         {QUEUE_GUIDELINES}",
    )
}

/// Appended to the CLI's own system prompt for the review stage's
/// reviewer sessions.
pub const REVIEWER_SYSTEM: &str = "You are the Reviewer agent in an autonomous build loop. \
Review and validate these claims. Find the gaps. \
You are a fresh-context combined review-and-fix agent: you did not build this work, and nothing about it is trusted until you verified it. \
You may read files, search the project, edit and write files and run shell commands. \
Do not modify the spec file, the task list or the project instruction file, and write no file other than your review report. \
First pass: thorough validation. Second pass: assume bugs remain and find what was missed.";

/// The review report every reviewer session must end its final message with
///: a verdict line and the findings JSON.
const REVIEW_REPORT: &str = "End your final message with the review report, and nothing after it:\n\n\
Verdict: PASS\n\n\
```json\n\
{\"high\": [], \"medium\": [], \"low\": []}\n\
```\n\n\
The verdict line is exactly `Verdict: PASS` when the work is validated and `Verdict: FAIL` when it is not. \
The JSON block holds one array per severity: high, medium and low. Each finding carries its severity (the bucket it sits in), the file, the line, the issue, whether you fixed it (\"fixed\": true or false), a category, the source evidence and a confidence between 0.0 and 1.0. \
An empty array is correct when you found nothing at that severity. The report is the last thing in your final message.";

/// The reviewer's job in order and the severity calibration, shared by the reviewer prompt variants.
const REVIEW_RULES: &str = "Your job, in order:\n\n\
1. Read the build claims (the fallback above when they are missing) and verify every claim against the code. The builder's claims are untrusted assertions: check each one against what the code actually does.\n\
2. Run the project's build and tests yourself, independently; never trust the builder's verification. Stack checks: Rust (cargo check, cargo clippy, cargo test), Python (compile and pytest), Node/TS (type-check and tests), Docker (config only, never start services); skip a check with a reason when the tool is unavailable.\n\
3. Scrutinize the Gaps and Assumptions: uncertainties and untested edge cases are where bugs hide.\n\
4. Fix every HIGH and MEDIUM issue you find, surgically, in the same pass: fix exactly the issue and nothing around it, no surrounding refactors, no separate fix loop. LOW issues are report-only.\n\
5. Re-run the checks after fixing.\n\n\
Severity calibration: HIGH (security holes, unhandled failures on external or user input, logic errors, crash paths) is always reported and fixed; MEDIUM (missing error handling at trust boundaries, resource leaks, off-by-one) is reported and fixed; LOW (style consistent with the codebase, local naming) is reported only. Borderlines: an unhandled error on user-controlled input is HIGH, not MEDIUM; an ignored return value with no production effect is LOW, not MEDIUM.\n\n\
Report bugs, panics, security issues, logic errors, missing boundary error handling, claims that contradict the code, races and leaks. Skip style, minor naming, missing docs, conventions matching the project and theoretical improvements.";

/// The user prompt of the review stage's reviewer session: a fresh-context combined review-and-fix agent. The
/// user prompt carries the task, the build claims (with the plan and the
/// changed-file list as the labelled fallback when the claims are missing),
/// the git diff when it is non-empty and at most 50 KB (otherwise the
/// changed-file list), the pass number and the spec and task file names.
pub fn reviewer(
    task: &Task,
    claims: Option<&str>,
    plan: Option<&str>,
    changed_files: &[String],
    diff: Option<&str>,
    pass: u32,
) -> String {
    let claims_block = match claims {
        Some(claims) => format!(
            "The build claims the builder wrote (untrusted; verify every claim):\n\n{claims}"
        ),
        None => format!(
            "No build claims were found; the plan the builder worked from and the changed files are the fallback:\n\n{plan}\n\nChanged files:\n{files}",
            plan = plan.unwrap_or("no plan was recorded"),
            files = file_list(changed_files),
        ),
    };
    let changes_block = match diff {
        Some(diff) => format!("The git diff of the work under review:\n\n```diff\n{diff}\n```"),
        None => format!(
            "The diff is empty or too large to carry; the changed files are:\n\n{files}",
            files = file_list(changed_files),
        ),
    };
    format!(
        "Review and validate the work done for the task below.\n\n\
         Task {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only. The spec is {SPEC_FILE} when it exists in the project root.\n\n\
         {claims_block}\n\n\
         {changes_block}\n\n\
         Pass {pass}.\n\n\
         {REVIEW_RULES}\n\n\
         {REVIEW_REPORT}",
        id = task.id,
        description = task.description,
    )
}

/// The per-file reviewer session of a multipass review: one file at a time, single-file concerns only, and report-only -- the
/// integration session decides what gets fixed.
pub fn reviewer_file(task: &Task, file: &str, pass: u32) -> String {
    format!(
        "Review one file of the work done for the task below.\n\n\
         Task {id}: {description}\n\n\
         The file to examine is exactly this one: {file}. Review it on its own merits: single-file concerns only (bugs, panics, security issues, logic errors, missing boundary error handling inside the file). Cross-file and integration concerns belong to the integration pass, not this one.\n\n\
         Do not edit any file: this pass is report-only. Every finding you report carries \"fixed\": false.\n\n\
         The spec is {SPEC_FILE} when it exists in the project root; the task list is {TASK_FILE}, both read-only.\n\n\
         Pass {pass}.\n\n\
         Report bugs, panics, security issues, logic errors and missing boundary error handling; skip style, minor naming, missing docs and conventions matching the project.\n\n\
         {REVIEW_REPORT}",
        id = task.id,
        description = task.description,
    )
}

/// The integration reviewer session of a multipass review: it receives the merged, de-duplicated per-file findings and
/// focuses on what per-file passes cannot see -- cross-file issues -- before
/// producing the final report.
pub fn reviewer_integration(
    task: &Task,
    findings_json: &str,
    changed_files: &[String],
    pass: u32,
) -> String {
    format!(
        "Review the integration of the work done for the task below; you are the final pass.\n\n\
         Task {id}: {description}\n\n\
         The per-file reviews already reported these findings (merged and de-duplicated):\n\n\
         ```json\n{findings_json}\n```\n\n\
         Do not re-report those findings; they are already recorded. Focus on what the per-file passes cannot see: cross-file issues, wiring, contracts between files, and anything in the gaps of the work.\n\n\
         The changed files are:\n{files}\n\n\
         The spec is {SPEC_FILE} when it exists in the project root; the task list is {TASK_FILE}, both read-only.\n\n\
         Pass {pass}.\n\n\
         {REVIEW_RULES}\n\n\
         {REVIEW_REPORT}",
        id = task.id,
        description = task.description,
        files = file_list(changed_files),
    )
}

/// One changed file per line, as a bulleted list.
fn file_list(files: &[String]) -> String {
    files
        .iter()
        .map(|file| format!("- {file}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Appended to the CLI's own system prompt for per-task plan sessions (T10.1).
pub const PLAN_SYSTEM: &str = "You are the Plan agent in an autonomous build loop. \
Before a task is implemented you write one implementation plan for a builder colleague who will carry it out. \
You may only read files; you have no shell. \
Edit no files: return the plan as your final message.";

/// The research report block a plan prompt carries as its prior artifact (T68.1):
/// input to verify, never a plan to copy.
fn research_block(research: Option<&str>) -> String {
    research
        .map(|research| {
            format!(
                "\n\nThe research report for this task (a prior artifact, not a plan):\n\n{research}\n\nUse it as input; verify anything you rely on."
            )
        })
        .unwrap_or_default()
}

pub fn plan(task: &Task, research: Option<&str>) -> String {
    format!(
        "Write one implementation plan for the task below, for a builder colleague to carry out.\n\n\
         Task {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only.{research}\n\n\
         The plan must end with these two sections, in this order:\n\n\
         ## File Operations\n\
         Every file to create or change, one per line, with what happens to each and why.\n\n\
         ## Verification\n\
         How to prove the work: the checks or commands to run and the result that means success.",
        id = task.id,
        description = task.description,
        research = research_block(research),
    )
}

/// The retry prompt after the plan gate rejected the first plan (T10.1): the new plan
/// must fix the rejection, so it names the reason and carries the rejected plan.
pub fn plan_retry(task: &Task, rejected: &str, reason: &str, research: Option<&str>) -> String {
    format!(
        "The plan you wrote for the task below was rejected. Write a new implementation plan for a builder colleague to carry out.\n\n\
         Task {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only.{research}\n\n\
         Why the previous plan was rejected:\n{reason}\n\n\
         The rejected plan:\n{rejected}\n\n\
         Write a new plan that fixes the rejection. The plan must end with these two sections, in this order:\n\n\
         ## File Operations\n\
         Every file to create or change, one per line, with what happens to each and why.\n\n\
         ## Verification\n\
         How to prove the work: the checks or commands to run and the result that means success.",
        id = task.id,
        description = task.description,
        research = research_block(research),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_prompt_carries_request_and_format_rules() {
        let p = planner("add a login page");
        assert!(p.contains("add a login page"));
        assert!(p.contains(TASK_FILE) && p.contains(SPEC_FILE));
        assert!(p.contains("last 20 lines"));
        assert!(p.contains("- [ ] T<N>.1: <description>"));
        assert!(p.contains("no markdown"));
    }

    #[test]
    fn discovery_prompt_carries_format_rules() {
        let p = discovery();
        assert!(p.contains(TASK_FILE) && p.contains(SPEC_FILE));
        assert!(p.contains("highest D number"));
        assert!(p.contains("- [ ] D<N>.1: <description>"));
        assert!(p.contains("no markdown"));
        assert!(p.contains("Do not modify existing tasks"));
    }

    fn test_task() -> Task {
        Task {
            id: "T8.1".into(),
            origin: Some('T'),
            description: "Add the per-task plan stage".into(),
            done: false,
            line: 1,
            raw: "- [ ] T8.1: Add the per-task plan stage".into(),
        }
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
    fn the_reviewer_system_prompt_states_the_stage_rules() {
        // The reviewer framing.
        assert!(REVIEWER_SYSTEM.contains("Review and validate these claims. Find the gaps."));
        assert!(REVIEWER_SYSTEM.contains("fresh-context combined review-and-fix agent"));
        // The two passes: thorough validation, then assuming bugs remain.
        assert!(REVIEWER_SYSTEM.contains("First pass: thorough validation"));
        assert!(
            REVIEWER_SYSTEM.contains("Second pass: assume bugs remain and find what was missed")
        );
        // The tool surface and the no-modify rules.
        assert!(REVIEWER_SYSTEM.contains("run shell commands"));
        assert!(REVIEWER_SYSTEM.contains("Do not modify the spec file, the task list"));
    }

    #[test]
    fn the_reviewer_prompt_carries_the_claims_the_diff_and_the_rules() {
        let claims = "## Files Changed\nCREATE a.txt\n\n## Claims\n- [ ] it works";
        let p = reviewer(
            &test_task(),
            Some(claims),
            None,
            &["src/a.rs".into(), "src/b.rs".into()],
            Some("+++ b/src/a.rs\n+hello"),
            1,
        );
        // The task id and description.
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        // The claims block, the diff and the pass number.
        assert!(p.contains(claims));
        assert!(p.contains("untrusted; verify every claim"));
        assert!(p.contains("```diff\n+++ b/src/a.rs\n+hello\n```"));
        assert!(p.contains("Pass 1."));
        // The spec and task file names.
        assert!(p.contains(SPEC_FILE) && p.contains(TASK_FILE));
        // The verify-independently, surgical-fix and no-separate-fix-loop rules.
        assert!(p.contains("never trust the builder's verification"));
        assert!(p.contains("independently"));
        assert!(p.contains("surgically, in the same pass"));
        assert!(p.contains("no separate fix loop"));
        assert!(p.contains("no surrounding refactors"));
        // The verdict line and the findings JSON contract.
        assert!(p.contains("Verdict: PASS"));
        assert!(p.contains("Verdict: FAIL"));
        assert!(p.contains("{\"high\": [], \"medium\": [], \"low\": []}"));
        assert!(p.contains("confidence between 0.0 and 1.0"));
        assert!(p.contains("whether you fixed it"));
        // The format rules are real newlines, not literal escape sequences.
        assert!(!p.contains("\\n"));
    }

    #[test]
    fn the_reviewer_prompt_falls_back_to_the_plan_and_the_file_list() {
        let p = reviewer(
            &test_task(),
            None,
            Some("## File Operations\nCREATE a.txt"),
            &["src/a.rs".into()],
            None,
            2,
        );
        assert!(p.contains("No build claims were found"));
        assert!(p.contains("## File Operations\nCREATE a.txt"));
        assert!(p.contains("- src/a.rs"));
        assert!(p.contains("The diff is empty or too large to carry"));
        assert!(p.contains("Pass 2."));
    }

    #[test]
    fn the_per_file_reviewer_prompt_is_report_only() {
        let p = reviewer_file(&test_task(), "src/a.rs", 1);
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        assert!(p.contains("exactly this one: src/a.rs"));
        assert!(p.contains("single-file concerns only"));
        // Report-only: no edits, every finding fixed false.
        assert!(p.contains("Do not edit any file: this pass is report-only"));
        assert!(p.contains("\"fixed\": false"));
        assert!(p.contains("Verdict: PASS"));
        assert!(p.contains(SPEC_FILE) && p.contains(TASK_FILE));
        assert!(!p.contains("verify every claim"));
    }

    #[test]
    fn the_integration_prompt_carries_the_merged_findings() {
        let p = reviewer_integration(
            &test_task(),
            "{\"high\": [\"a leak\"], \"medium\": [], \"low\": []}",
            &["src/a.rs".into()],
            2,
        );
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        assert!(p.contains("{\"high\": [\"a leak\"], \"medium\": [], \"low\": []}"));
        assert!(p.contains("Do not re-report those findings"));
        assert!(p.contains("cross-file issues"));
        assert!(p.contains("- src/a.rs"));
        // It reviews and fixes like the main pass: the shared rules travel along.
        assert!(p.contains("never trust the builder's verification"));
        assert!(p.contains("no separate fix loop"));
        assert!(p.contains("Verdict: PASS"));
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
}
