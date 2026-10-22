//! The pieces shared by the CLI providers: preview constants, first-key helpers and the
//! generic CLI-provider skeleton (availability check, session run, JSONL line parse) that
//! the Codex and Vibe providers are thin over. The Claude provider adopts only the
//! low-level pieces; `process` stays the single spawn path.

use patok_core::event::{AgentEvent, preview};
use serde_json::Value;

use crate::process::{self, LineParser, Parsed, ProcessSpec};
use crate::{ProviderError, ProviderInfo, SessionRequest, SessionResult, Tool, cli_in};

/// Tool-call inputs are truncated to this many characters.
pub(crate) const TOOL_PREVIEW: usize = 120;
/// Tool results and errors are truncated to this many characters.
pub(crate) const RESULT_PREVIEW: usize = 200;

/// The first of `keys` present as a string.
pub(crate) fn first_str<'a>(value: &'a Value, keys: &[&'a str]) -> Option<&'a str> {
    keys.iter().find_map(|k| value[*k].as_str())
}

/// The first of `keys` present as a non-empty string.
pub(crate) fn first_non_empty<'a>(value: &'a Value, keys: &[&'a str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| value[*k].as_str().filter(|s| !s.is_empty()))
}

/// The first of `keys` present as a string, the whole input as JSON otherwise, previewed.
pub(crate) fn preview_input(input: &Value, keys: &[&str]) -> String {
    let text = first_str(input, keys).map_or_else(|| input.to_string(), str::to_string);
    preview(&text, TOOL_PREVIEW)
}

/// A failed tool surfaces as a stderr event: its output, or "name status" when it has none.
pub(crate) fn failed_tool_stderr(name: &str, status: &str, output: &str) -> AgentEvent {
    AgentEvent::Stderr {
        text: preview(
            &if output.is_empty() {
                format!("{name} {status}")
            } else {
                output.to_string()
            },
            RESULT_PREVIEW,
        ),
    }
}

/// The prompt with the system prompt prepended, for CLIs with no system-prompt flag.
pub(crate) fn prepend_system_prompt(request: &SessionRequest) -> String {
    match request.system_prompt.as_deref().filter(|s| !s.is_empty()) {
        Some(system) => format!("{system}\n\n{}", request.prompt),
        None => request.prompt.clone(),
    }
}

/// Maps each allowlist tool through `map`, dropping unmapped names and duplicates.
pub(crate) fn map_tools(
    allowed: &[Tool],
    map: impl Fn(Tool) -> &'static [&'static str],
) -> Vec<&'static str> {
    let mut tools: Vec<&'static str> = vec![];
    for tool in allowed {
        for name in map(*tool) {
            if !tools.contains(name) {
                tools.push(name);
            }
        }
    }
    tools
}

/// One JSONL stdout line: a JSON object goes to `events_for`, an empty line is skipped,
/// anything else surfaces as a stderr event.
pub(crate) fn parse_line(line: &str, events_for: impl Fn(&Value) -> Vec<AgentEvent>) -> Parsed {
    let line = line.trim();
    match serde_json::from_str::<Value>(line) {
        Ok(value) if value.is_object() => Parsed {
            events: events_for(&value),
            json: true,
        },
        _ if line.is_empty() => Parsed {
            events: vec![],
            json: false,
        },
        _ => Parsed {
            events: vec![AgentEvent::Stderr { text: line.into() }],
            json: false,
        },
    }
}

/// Implements `LineParser` for a parser that already exposes `parse`.
macro_rules! forward_line_parser {
    ($parser:ty) => {
        impl $crate::process::LineParser for $parser {
            fn parse_line(&mut self, line: &str) -> $crate::process::Parsed {
                self.parse(line)
            }
        }
    };
}
pub(crate) use forward_line_parser;

/// What differs between the JSONL CLI providers; everything else they do is shared here.
pub(crate) trait CliSpec {
    /// This provider's catalog entry: its slug and CLI binary.
    const INFO: &'static ProviderInfo;
    /// The provider's line parser.
    type Parser: LineParser + Default;
    /// The full CLI argument list for one session.
    fn build_args(request: &SessionRequest) -> Vec<String>;
}

/// The availability check for a CLI named `cli`: `Err(CliNotFound)` naming it
/// when it is not on `PATH`. Run before a session starts.
pub(crate) fn check_cli_on_path(cli: &str) -> Result<(), ProviderError> {
    check_available_in(cli, std::env::var_os("PATH").as_deref())
}

/// The availability check for a CLI named `cli` against an explicit `PATH`
/// value.
pub(crate) fn check_available_in(
    cli: &str,
    path: Option<&std::ffi::OsStr>,
) -> Result<(), ProviderError> {
    if cli_in(cli, path) {
        Ok(())
    } else {
        Err(ProviderError::CliNotFound { cli: cli.into() })
    }
}

/// Fails with a clear error naming the CLI when it is not on `PATH`. Run before a
/// session starts.
pub(crate) fn check_available<S: CliSpec>() -> Result<(), ProviderError> {
    check_cli_on_path(S::INFO.binary)
}

/// Runs one session: check availability, spawn the CLI, stream its JSONL output through
/// the provider's parser. The prompt goes on the command line and the environment is
/// inherited, the shape the Codex and Vibe CLIs need.
pub(crate) async fn run_session<S: CliSpec>(
    request: SessionRequest,
) -> Result<SessionResult, ProviderError> {
    check_available::<S>()?;
    let spec = ProcessSpec {
        program: S::INFO.binary.into(),
        args: S::build_args(&request),
        env: vec![],
        stdin: String::new(),
    };
    let model = request.model.clone().unwrap_or_default();
    process::run(spec, &request, S::Parser::default(), S::INFO.slug, &model).await
}
