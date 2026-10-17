//! The group-completion push: `git push <auto_push_remote> HEAD`.

use patok_core::event::NoticeLevel;

use crate::context::SessionContext;

/// Pushes HEAD to the `auto_push_remote` remote once a task group completes.
/// The setting is read live; empty means no push, and outside a git
/// repository nothing happens. A failure is a warning carrying git's error
/// only: the commits stay local and the build loop continues.
pub(crate) struct PushStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> PushStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    pub(crate) async fn run(&self) {
        let ctx = self.ctx;
        let remote = ctx.auto_push_remote();
        if remote.is_empty() {
            return;
        }
        let git = ctx.git();
        if !git.is_repo().await {
            return;
        }
        match git.push(&remote).await {
            Ok(()) => ctx.notice(NoticeLevel::Info, format!("Pushed to {remote}")),
            Err(e) => ctx.notice(
                NoticeLevel::Warning,
                format!("Push to {remote} failed; the commits stay local: {e}"),
            ),
        }
    }
}
