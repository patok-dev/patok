use patok_core::event::{AgentRole, NoticeLevel};

use crate::context::SessionContext;
use crate::task_list::AppendMode;

/// The shared post-run check of an append-style run (T180.4, section 9 of
/// docs/private/engine-restructure.md): the validation the planner,
/// queue-creation and discovery runs share after their session completed.
/// This step runs no agent.
pub(crate) struct ValidateAppendStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> ValidateAppendStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// The shared post-run check of an append-style run: the whole check --
    /// read, validate and, on a violation, restore, with the lines injected
    /// through `inject_task` during the session kept out of the accounting
    /// (T77.1) -- runs in the task list under its file lock. On violation an
    /// error is reported; otherwise the number of tasks added is. Returns
    /// the IDs that were appended, or the rejection reason on a violation
    /// (the queue-creation run saves its research report only on a pass, so
    /// a rejected run re-runs). Recording the appended IDs as this session's
    /// UI-added task IDs is the caller's decision (`record_ui_added`).
    pub(crate) async fn run(
        &self,
        before: &str,
        mode: AppendMode,
        role: AgentRole,
        record_ui_added: bool,
    ) -> Result<Vec<String>, String> {
        match self
            .ctx
            .task_list()
            .check_appended(before, mode, role)
            .await
        {
            Ok(ids) => {
                if record_ui_added {
                    self.ctx.task_list().record_ui_added(&ids);
                }
                match ids.len() {
                    0 => self.ctx.notice(NoticeLevel::Info, "No tasks added."),
                    1 => self.ctx.notice(NoticeLevel::Info, "1 task added."),
                    n => self
                        .ctx
                        .notice(NoticeLevel::Info, format!("{n} tasks added.")),
                }
                Ok(ids)
            }
            Err(rejection) => {
                let restored = if rejection.restored {
                    "the previous task file was restored"
                } else {
                    "restoring the previous task file failed"
                };
                self.ctx.notice(
                    NoticeLevel::Error,
                    format!("{role} result rejected: {}; {restored}.", rejection.reason),
                );
                Err(rejection.reason)
            }
        }
    }
}
