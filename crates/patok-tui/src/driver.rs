//! The plumbing shared by the two drivers (finding 22): decoding the engine's
//! update stream and reporting a submit command's verdict. The interactive
//! driver (`run.rs`) and the headless driver (`headless.rs`) both consume the
//! attach stream through [`next_update`] and submit commands through
//! [`submit`], so the stream protocol is decoded in exactly one place.

use patok_core::event::EngineEvent;
use patok_proto::{CommandRequest, command_request, engine_update};
use tonic::Streaming;

/// One decoded item from the engine's update stream.
pub(crate) enum Update {
    /// An event that decoded cleanly; the driver applies it to its state.
    Event(EngineEvent),
    /// An event whose payload failed to decode: the proto event's `kind`
    /// and the decode error, for the driver's unreadable-event report.
    UnreadableEvent { kind: String, error: String },
    /// The engine announced its shutdown; the string is the reason.
    ShutdownNotice(String),
    /// A payload both drivers ignore (the attach `Info`, a
    /// `RunStateUpdate`, or no payload).
    Ignored,
    /// The stream ended; the engine is gone.
    StreamEnded,
    /// The stream failed; the string is the transport's message.
    ConnectionLost(String),
}

/// Decodes the next item from the engine's update stream: the two-level match
/// (tonic's message result, then the payload oneof) both drivers ran
/// separately. tonic's `Streaming` has no public constructor, so the Ok
/// paths stay covered by the e2e and headless integration tests.
pub(crate) async fn next_update(updates: &mut Streaming<patok_proto::EngineUpdate>) -> Update {
    match updates.message().await {
        Ok(Some(update)) => match update.payload {
            Some(engine_update::Payload::Event(event)) => {
                match EngineEvent::from_payload(&event.payload) {
                    Ok(event) => Update::Event(event),
                    Err(error) => Update::UnreadableEvent {
                        kind: event.kind,
                        error: error.to_string(),
                    },
                }
            }
            Some(engine_update::Payload::ShutdownNotice(notice)) => {
                Update::ShutdownNotice(notice.reason)
            }
            _ => Update::Ignored,
        },
        Ok(None) => Update::StreamEnded,
        Err(status) => Update::ConnectionLost(status.message().to_string()),
    }
}

/// The submit RPC's result, as the drivers and their dispatches ask the
/// engine for it.
pub(crate) type CommandCall = Result<tonic::Response<patok_proto::CommandResponse>, tonic::Status>;

/// Submits one command through `call` and reports the engine's verdict:
/// `Ok(())` when accepted; the rejection reason as-is, or the transport
/// failure prefixed `"{failure_prefix}: {message}"`, otherwise. Generic
/// over the call so tests pass fakes, like the run.rs dispatches.
pub(crate) async fn submit<Req, Fut>(
    call: Req,
    action: command_request::Action,
    failure_prefix: &str,
) -> Result<(), String>
where
    Req: FnOnce(CommandRequest) -> Fut,
    Fut: std::future::Future<Output = CommandCall>,
{
    let request = CommandRequest {
        action: Some(action),
    };
    match call(request).await {
        Ok(response) => {
            let response = response.into_inner();
            if response.accepted {
                Ok(())
            } else {
                Err(response.error)
            }
        }
        Err(status) => Err(format!("{failure_prefix}: {}", status.message())),
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandCall, submit};
    use patok_proto::{CommandRequest, StartBuild, command_request};

    /// A submit fake that answers with the given outcome: an accepting
    /// response when `accepted`, a rejected one carrying `error` otherwise.
    fn answering(
        accepted: bool,
        error: &'static str,
    ) -> impl FnOnce(CommandRequest) -> std::future::Ready<CommandCall> {
        move |_command| {
            std::future::ready(Ok(tonic::Response::new(patok_proto::CommandResponse {
                accepted,
                error: error.into(),
                ..Default::default()
            })))
        }
    }

    /// A submit fake that always fails with the given status.
    fn failing(
        message: &'static str,
    ) -> impl FnOnce(CommandRequest) -> std::future::Ready<CommandCall> {
        move |_command| {
            let answer: CommandCall = Err(tonic::Status::unavailable(message));
            std::future::ready(answer)
        }
    }

    /// An accepted command reports success.
    #[tokio::test]
    async fn an_accepted_command_reports_success() {
        let outcome = submit(
            answering(true, ""),
            command_request::Action::StartBuild(StartBuild {}),
            "start failed",
        )
        .await;
        assert_eq!(outcome, Ok(()));
    }

    /// A rejection reports the engine's reason verbatim, with no prefix.
    #[tokio::test]
    async fn a_rejection_reports_the_engine_reason_verbatim() {
        let outcome = submit(
            answering(false, "planner busy"),
            command_request::Action::StartBuild(StartBuild {}),
            "start failed",
        )
        .await;
        assert_eq!(outcome, Err("planner busy".into()));
    }

    /// A transport failure reports the prefixed status message.
    #[tokio::test]
    async fn a_transport_failure_reports_the_prefixed_message() {
        let outcome = submit(
            failing("nope"),
            command_request::Action::StartBuild(StartBuild {}),
            "start failed",
        )
        .await;
        assert_eq!(outcome, Err("start failed: nope".into()));
    }
}
