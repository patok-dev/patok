//! The shared `SessionRequest` fixture of the provider unit tests.

use std::time::Duration;

use patok_core::event::AgentEvent;
use tokio::sync::mpsc;

use crate::{SessionRequest, Tool};

/// A `SessionRequest` with the given prompt, system prompt and tool allowlist, a
/// fresh event channel (its receiver is returned for tests that read the stream),
/// and inert defaults for everything else: `.` directories, a raw log path of `./t-raw.jsonl`,
/// a one-second idle timeout and a fresh cancellation token.
pub(crate) fn request(
    prompt: &str,
    system_prompt: Option<&str>,
    allowed_tools: &[Tool],
) -> (SessionRequest, mpsc::UnboundedReceiver<AgentEvent>) {
    let (events, rx) = mpsc::unbounded_channel();
    (
        SessionRequest {
            model: None,
            prompt: prompt.into(),
            system_prompt: system_prompt.map(str::to_string),
            project_dir: ".".into(),
            events,
            raw_log_path: "t-raw.jsonl".into(),
            idle_timeout: Duration::from_secs(1),
            cancel: Default::default(),
            allowed_tools: allowed_tools.to_vec(),
        },
        rx,
    )
}
