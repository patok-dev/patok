//! The shared announce-run-classify skeleton every agent step runs on.

use std::fs;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use patok_core::event::{AgentEvent, AgentRole, EngineEvent, NoticeLevel, SessionOutcome};
use patok_providers::{ExitKind, Provider, ProviderError, SessionRequest, SessionResult, Tool};
use tokio::sync::mpsc;

use crate::agents::Agent;
use crate::context::{Pause, SessionContext};

/// One agent session's prompt bundle: the user prompt, the system prompt, the transcript
/// label and the tool allowlist.
pub(crate) struct AgentPrompt {
    pub(crate) prompt: String,
    pub(crate) system_prompt: &'static str,
    pub(crate) label: String,
    /// The session's tool allowlist, provider-neutral; the provider maps each
    /// entry to its own CLI's tool name(s).
    pub(crate) tools: &'static [Tool],
}

impl AgentPrompt {
    /// One agent's prompt bundle, straight from its [`Agent`] interface.
    pub(crate) fn of(agent: &impl Agent) -> Self {
        Self {
            prompt: agent.user_prompt(),
            system_prompt: agent.system_prompt(),
            label: agent.label(),
            tools: agent.tools(),
        }
    }
}

/// How the shared stage session's starting line is produced (finding 17 in
/// docs/private/structure-review.md): five stages announce inside
/// [`run_stage`]; the planner and queue-creation runs were announced
/// by their `start_*` under the state guard, before the planning flag
/// flipped (T24.1), and pass that instant.
pub(crate) enum StageStart {
    /// Announce now, as the active agent.
    Announce,
    /// Already announced; the recorded instant the duration is measured from.
    Announced(Instant),
}

/// The stage-specific words of the shared announce-run-match-exit skeleton
/// (finding 17 in docs/private/structure-review.md): every stage reports a
/// session that could not start with the provider's error, a cancelled one
/// with the stage's warning (the reviewer session stays silent), and a failed
/// one with the stage's level and wording (the research stage's failure is a
/// non-blocking warning; every other stage's is an error).
pub(crate) struct StageWords {
    /// Whether the stage reads the session's normalised events (the plan
    /// stage reads the plan out of them, T10.1); the exit's `Completed`
    /// carries them only when this is on.
    pub(crate) capture: bool,
    /// The tracing subject, e.g. "planner could not start" or
    /// "agent could not start for T42.1".
    pub(crate) could_not_start: String,
    /// The failure reason when the provider reported none.
    pub(crate) default_failure: String,
    /// The cancelled-session warning, or `None` to stay silent.
    pub(crate) cancelled_notice: Option<String>,
    /// The failed-session notice level.
    pub(crate) failure_level: NoticeLevel,
    /// The failed-session notice around the reason.
    pub(crate) failure_prefix: String,
    pub(crate) failure_suffix: String,
}

/// One stage session's classified exit (finding 17 in
/// docs/private/structure-review.md): the caller continues with the
/// stage-specific post-processing.
pub(crate) enum StageExit {
    /// Completed; the captured events when the stage asked for capture.
    Completed(Vec<AgentEvent>),
    /// Cancelled; the stage's warning, when it has one, was reported.
    Cancelled,
    /// Failed or could not start; the failure notice was reported.
    Failed,
}

/// Runs one agent session on the given provider: its normalised events are streamed
/// to attached shells and its raw stream is written to the history log. With `capture`,
/// the normalised events are also collected and returned (the plan stage reads the plan
/// out of them, T10.1). Returns the session's outcome, the captured events, the
/// base directory and the sequence number.
async fn run_agent_session(
    ctx: &SessionContext,
    provider: &Arc<dyn Provider>,
    model: Option<&str>,
    agent: AgentPrompt,
    capture: bool,
) -> (
    Result<SessionResult, ProviderError>,
    Vec<AgentEvent>,
    std::path::PathBuf,
    u32,
) {
    let (events, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let emit = ctx.event_sink();
    let forwarder = tokio::spawn(async move {
        let mut captured = Vec::new();
        while let Some(event) = rx.recv().await {
            if capture {
                captured.push(event.clone());
            }
            emit(EngineEvent::Agent { event });
        }
        captured
    });

    let base_dir = ctx.current_task_dir();
    let seq = ctx.next_sequence_number();
    let raw_log_path = base_dir.join(format!(
        "{:03}-{}-{}-raw.jsonl",
        seq,
        agent.label,
        provider.slug()
    ));
    let session = provider
        .run_session(SessionRequest {
            model: model.map(str::to_string),
            prompt: agent.prompt,
            system_prompt: Some(agent.system_prompt.to_string()),
            project_dir: ctx.project_dir().to_path_buf(),
            events,
            raw_log_path,
            idle_timeout: ctx.agent_timeout(),
            cancel: ctx.cancel_token().clone(),
            allowed_tools: agent.tools.to_vec(),
        })
        .await;
    // The request (and its sender) is gone: let the forwarder flush the tail of the stream.
    let captured = forwarder.await.unwrap_or_default();
    (session, captured, base_dir, seq)
}

/// Runs one stage session through the shared announce-run-match-exit
/// skeleton (finding 17 in docs/private/structure-review.md): the agent
/// is announced as the active agent (or its already-recorded start
/// instant is used when the `start_*` command announced it under the
/// state guard, T24.1), the session runs on the given provider and
/// model -- not the context's own, so the review stage can run on the
/// reviewer provider -- and one finished line lands
/// after the agent output and before the stage's own outcome notices
/// (T42.1). A session that could not start reports the provider's
/// error, a cancelled one the stage's warning (the reviewer session
/// stays silent), and a failed one the stage's level and wording.
/// Returns the stage exit and a result record that the caller must write
/// with the step's outcome.
pub(crate) async fn run_stage(
    ctx: &SessionContext,
    provider: &Arc<dyn Provider>,
    model: Option<&str>,
    start: StageStart,
    role: AgentRole,
    agent: AgentPrompt,
    words: StageWords,
) -> (StageExit, ResultRecord) {
    let agent_label = agent.label.clone();
    let start = match start {
        StageStart::Announce => {
            ctx.pause(Pause::Agent).await;
            ctx.announce_agent(role, provider.slug(), model)
        }
        StageStart::Announced(start) => start,
    };
    let (session, events, base_dir, seq) =
        run_agent_session(ctx, provider, model, agent, words.capture).await;
    ctx.record_agent(session.as_ref().is_ok_and(|result| result.rate_limited));
    ctx.finish_agent(role, start, session_outcome(&session));

    // Build the result record from the session
    let record = match &session {
        Err(error) => {
            tracing::error!("{}: {error}", words.could_not_start);
            ctx.notice(NoticeLevel::Error, error.to_string());
            ResultRecord {
                path: base_dir.join(format!("{:03}-{}-result.json", seq, agent_label)),
                agent: agent_label,
                provider: provider.slug().to_string(),
                model: model.map(str::to_string),
                exit_kind: ExitKind::Failed,
                success: false,
                failure: Some(error.to_string()),
            }
        }
        Ok(result) if result.exit == ExitKind::Completed => ResultRecord {
            path: base_dir.join(format!("{:03}-{}-result.json", seq, agent_label)),
            agent: agent_label,
            provider: provider.slug().to_string(),
            model: model.map(str::to_string),
            exit_kind: result.exit,
            success: result.success,
            failure: result.failure.clone(),
        },
        Ok(result) if result.exit == ExitKind::Cancelled => {
            if let Some(text) = &words.cancelled_notice {
                ctx.notice(NoticeLevel::Warning, text.clone());
            }
            ResultRecord {
                path: base_dir.join(format!("{:03}-{}-result.json", seq, agent_label)),
                agent: agent_label,
                provider: provider.slug().to_string(),
                model: model.map(str::to_string),
                exit_kind: result.exit,
                success: result.success,
                failure: result.failure.clone(),
            }
        }
        Ok(result) => {
            let reason = result
                .failure
                .as_deref()
                .unwrap_or(words.default_failure.as_str());
            ctx.notice(
                words.failure_level,
                format!("{}{reason}{}", words.failure_prefix, words.failure_suffix),
            );
            ResultRecord {
                path: base_dir.join(format!("{:03}-{}-result.json", seq, agent_label)),
                agent: agent_label,
                provider: provider.slug().to_string(),
                model: model.map(str::to_string),
                exit_kind: result.exit,
                success: result.success,
                failure: result.failure.clone(),
            }
        }
    };

    let exit = match session {
        Err(_) => StageExit::Failed,
        Ok(result) if result.exit == ExitKind::Completed => StageExit::Completed(events),
        Ok(result) if result.exit == ExitKind::Cancelled => StageExit::Cancelled,
        Ok(_) => StageExit::Failed,
    };

    (exit, record)
}

/// The finished line's outcome of one agent session.
fn session_outcome(session: &Result<SessionResult, ProviderError>) -> SessionOutcome {
    match session {
        Err(_) => SessionOutcome::Failed,
        Ok(result) => match result.exit {
            ExitKind::Completed => SessionOutcome::Finished,
            ExitKind::Cancelled => SessionOutcome::Cancelled,
            ExitKind::Failed | ExitKind::TimedOut => SessionOutcome::Failed,
        },
    }
}

/// The `<seq>-<agent>-result.json` record of one stage session, filled by
/// [`run_stage`] from the session's exit; the caller adds the step's outcome.
#[must_use = "every stage session writes its result record"]
pub(crate) struct ResultRecord {
    pub(crate) path: std::path::PathBuf,
    pub(crate) agent: String,
    pub(crate) provider: String,
    pub(crate) model: Option<String>,
    pub(crate) exit_kind: ExitKind,
    pub(crate) success: bool,
    pub(crate) failure: Option<String>,
}

impl ResultRecord {
    /// Sets the failure text when the provider reported none (a rejected plan, an empty report).
    pub(crate) fn failure(mut self, text: impl Into<String>) -> Self {
        self.failure.get_or_insert_with(|| text.into());
        self
    }

    /// Writes the record with the step's outcome next to the raw log; I/O errors are swallowed.
    pub(crate) fn write(&self, outcome: &impl Serialize) {
        #[derive(Serialize)]
        struct Record<'a, T> {
            agent: &'a str,
            provider: &'a str,
            model: Option<&'a str>,
            exit_kind: ExitKind,
            success: bool,
            failure: Option<&'a str>,
            outcome: &'a T,
        }
        let record = Record {
            agent: &self.agent,
            provider: &self.provider,
            model: self.model.as_deref(),
            exit_kind: self.exit_kind,
            success: self.success,
            failure: self.failure.as_deref(),
            outcome,
        };
        if let Ok(json) = serde_json::to_string_pretty(&record) {
            let _ = fs::write(&self.path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patok_providers::ExitKind;
    use serde::Serialize;

    #[derive(Serialize)]
    struct BuildOutcome {
        completed: bool,
        claims: Option<String>,
    }

    #[test]
    fn result_record_writes_correct_file() {
        let dir = tempfile::tempdir().unwrap();
        let record = ResultRecord {
            path: dir.path().join("007-T1.1-result.json"),
            agent: "T1.1".to_string(),
            provider: "test-provider".to_string(),
            model: Some("test-model".to_string()),
            exit_kind: ExitKind::Completed,
            success: true,
            failure: None,
        };
        let outcome = BuildOutcome {
            completed: true,
            claims: Some("claims text".to_string()),
        };
        record.write(&outcome);

        let path = dir.path().join("007-T1.1-result.json");
        assert!(path.exists(), "result file should exist");
        let content = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(json["agent"], "T1.1");
        assert_eq!(json["provider"], "test-provider");
        assert_eq!(json["model"], "test-model");
        assert_eq!(json["exit_kind"], "Completed");
        assert_eq!(json["success"], true);
        assert!(json["failure"].is_null());
        assert_eq!(json["outcome"]["completed"], true);
        assert_eq!(json["outcome"]["claims"], "claims text");
    }

    #[test]
    fn result_record_failure_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let record = ResultRecord {
            path: dir.path().join("001-plan-result.json"),
            agent: "plan".to_string(),
            provider: "test-provider".to_string(),
            model: None,
            exit_kind: ExitKind::Completed,
            success: true,
            failure: Some("original failure".to_string()),
        };
        // Using .failure() should not overwrite existing failure
        let record = record.failure("new failure");
        assert_eq!(record.failure, Some("original failure".to_string()));
    }

    #[test]
    fn result_record_failure_sets_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let record = ResultRecord {
            path: dir.path().join("001-plan-result.json"),
            agent: "plan".to_string(),
            provider: "test-provider".to_string(),
            model: None,
            exit_kind: ExitKind::Completed,
            success: true,
            failure: None,
        };
        // Using .failure() should set when None
        let record = record.failure("new failure");
        assert_eq!(record.failure, Some("new failure".to_string()));
    }
}
