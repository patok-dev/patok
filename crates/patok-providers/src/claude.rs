//! The Claude provider: drives the `claude` CLI in print mode and normalises its
//! stream-json output.

use async_trait::async_trait;
use patok_core::event::{AgentEvent, Usage, preview};
use serde_json::Value;

use crate::catalog::CLAUDE;
use crate::cli;
use crate::process::{self, Parsed, ProcessSpec};
use crate::{DiscoveredModel, Provider, ProviderError, SessionRequest, SessionResult, Tool};

const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// Print mode with stream-json on both ends, so the CLI answers control requests on stdout.
const DISCOVERY_ARGS: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--no-session-persistence",
];
/// The control request whose response lists the models.
const INITIALIZE_REQUEST: &str =
    r#"{"type":"control_request","request_id":"r1","request":{"subtype":"initialize"}}"#;
const INITIALIZE_REQUEST_ID: &str = "r1";

pub struct Claude;

#[async_trait]
impl Provider for Claude {
    fn slug(&self) -> &'static str {
        CLAUDE.slug
    }

    fn supports_model_selection(&self) -> bool {
        CLAUDE.supports_model_selection
    }

    fn check_available(&self) -> Result<(), ProviderError> {
        cli::check_cli_on_path(CLAUDE.binary)
    }

    async fn run_session(&self, request: SessionRequest) -> Result<SessionResult, ProviderError> {
        let spec = ProcessSpec {
            program: CLAUDE.binary.into(),
            args: build_args(&request),
            // Marks a nested Claude session; emptied so the child does not refuse to start.
            env: vec![("CLAUDECODE", "")],
            stdin: request.prompt.clone(),
        };
        let model = request.model.clone().unwrap_or_default();
        process::run(spec, &request, StreamParser::default(), self.slug(), &model).await
    }

    async fn discover_models(&self) -> Result<Vec<DiscoveredModel>, ProviderError> {
        discover_with(discovery_spec()).await
    }
}

fn discovery_spec() -> ProcessSpec {
    ProcessSpec {
        program: CLAUDE.binary.into(),
        args: DISCOVERY_ARGS.iter().map(|a| (*a).to_string()).collect(),
        // Marks a nested Claude session; emptied so the child does not refuse to start.
        env: vec![("CLAUDECODE", "")],
        stdin: format!("{INITIALIZE_REQUEST}\n"),
    }
}

/// Runs `spec` as the discovery process; tests substitute the spec to mock the spawn.
async fn discover_with(spec: ProcessSpec) -> Result<Vec<DiscoveredModel>, ProviderError> {
    // Discovery is not about a project; a neutral directory keeps project settings and hooks
    // out of the harness.
    let cwd = std::env::temp_dir();
    let discovery_error = |reason: String| ProviderError::Discovery {
        provider: CLAUDE.slug.into(),
        reason,
    };
    match process::exchange(&spec, &cwd, parse_initialize_line).await? {
        Some(Ok(models)) => Ok(models),
        Some(Err(reason)) => Err(discovery_error(reason)),
        None => Err(discovery_error(
            "the harness exited without answering the initialize request".into(),
        )),
    }
}

/// What one stdout line of the discovery process says about the `initialize` request:
/// `None` for any other line, otherwise the listed models or why there are none. Reads only
/// `models`; the rest of the response (account data) is never inspected, kept or logged.
pub fn parse_initialize_line(line: &str) -> Option<Result<Vec<DiscoveredModel>, String>> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value["type"] != "control_response" {
        return None;
    }
    let outer = &value["response"];
    if outer["request_id"]
        .as_str()
        .is_some_and(|id| id != INITIALIZE_REQUEST_ID)
    {
        return None;
    }
    if outer["subtype"] == "error" {
        return Some(Err(format!(
            "the harness rejected the initialize request: {}",
            outer["error"].as_str().unwrap_or("no reason given")
        )));
    }
    // Only `models` of the initialize payload (`response.response`) is read; the rest of the
    // payload carries account data and is never inspected.
    let Some(entries) = outer["response"]["models"].as_array() else {
        return Some(Err("the initialize response has no models list".into()));
    };
    let mut models: Vec<DiscoveredModel> = vec![];
    for entry in entries {
        let (id, display_name) = match entry {
            Value::String(s) => (s.as_str(), None),
            Value::Object(_) => match entry["value"].as_str() {
                Some(id) => (id, entry["displayName"].as_str().map(String::from)),
                None => continue,
            },
            _ => continue,
        };
        if id.is_empty() || models.iter().any(|m| m.id == id) {
            continue;
        }
        models.push(DiscoveredModel {
            id: id.into(),
            display_name,
            listed: true,
        });
    }
    if models.is_empty() {
        return Some(Err("the harness reported no models".into()));
    }
    Some(Ok(models))
}

fn build_args(request: &SessionRequest) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]
    .map(String::from)
    .into();
    if let Some(model) = request.model.as_deref().filter(|m| !m.is_empty()) {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(system) = request.system_prompt.as_deref().filter(|s| !s.is_empty()) {
        args.extend(["--append-system-prompt".into(), system.into()]);
    }
    if !request.allowed_tools.is_empty() {
        let tools: Vec<&str> = request
            .allowed_tools
            .iter()
            .flat_map(|tool| claude_tools(*tool).iter().copied())
            .collect();
        args.extend(["--allowedTools".into(), tools.join(",")]);
    }
    args
}

/// Maps a role allowlist tool to the Claude CLI's own name for it.
fn claude_tools(tool: Tool) -> &'static [&'static str] {
    match tool {
        Tool::Bash => &["Bash"],
        Tool::Edit => &["Edit"],
        Tool::Write => &["Write"],
        Tool::Read => &["Read"],
        Tool::Glob => &["Glob"],
        Tool::Grep => &["Grep"],
        Tool::NotebookEdit => &["NotebookEdit"],
        Tool::WebFetch => &["WebFetch"],
        Tool::WebSearch => &["WebSearch"],
    }
}

/// Translates Claude's stream-json lines into normalised events. Stateful: text arrives twice
/// (partial deltas, then the completed message), and usage reports the last turn's input.
#[derive(Default)]
pub struct StreamParser {
    delta_emitted: bool,
    last_turn_input: Option<u64>,
    /// A rate-limit event with a status other than `allowed` was seen.
    rate_limited: bool,
}

impl crate::process::LineParser for StreamParser {
    fn parse_line(&mut self, line: &str) -> Parsed {
        self.parse(line)
    }

    fn rate_limited(&self) -> bool {
        self.rate_limited
    }
}

impl StreamParser {
    pub fn parse(&mut self, line: &str) -> Parsed {
        let line = line.trim();
        if line.is_empty() {
            return Parsed {
                events: vec![],
                json: false,
            };
        }
        let value = serde_json::from_str::<Value>(line).ok().or_else(|| {
            let stripped = strip_escapes(line);
            serde_json::from_str::<Value>(stripped.trim()).ok()
        });
        match value {
            Some(value) => Parsed {
                events: self.events_for(&value),
                json: true,
            },
            None => Parsed {
                events: non_json(line),
                json: false,
            },
        }
    }

    fn events_for(&mut self, value: &Value) -> Vec<AgentEvent> {
        match value["type"].as_str().unwrap_or_default() {
            "stream_event" => self.stream_event(&value["event"]),
            "assistant" => self.assistant(&value["message"]),
            "user" => user(value),
            "system" => system(value),
            "error" => vec![AgentEvent::Stderr {
                text: error_text(value),
            }],
            "result" => self.result(value),
            "rate_limit_event" => self.rate_limit(value),
            other => vec![AgentEvent::Text {
                text: format!("[{other}]"),
            }],
        }
    }

    fn rate_limit(&mut self, value: &Value) -> Vec<AgentEvent> {
        let status = value["rate_limit_info"]["status"]
            .as_str()
            .unwrap_or("unknown");
        if status == "allowed" {
            return vec![];
        }
        self.rate_limited = true;
        vec![AgentEvent::Text {
            text: format!("[rate limited] {status}"),
        }]
    }

    fn stream_event(&mut self, event: &Value) -> Vec<AgentEvent> {
        match event["type"].as_str() {
            Some("message_start") => {
                self.delta_emitted = false;
                vec![]
            }
            Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                let text = event["delta"]["text"].as_str().unwrap_or_default();
                if text.is_empty() {
                    return vec![];
                }
                self.delta_emitted = true;
                vec![AgentEvent::TextDelta {
                    text: text.to_string(),
                }]
            }
            _ => vec![],
        }
    }

    fn assistant(&mut self, message: &Value) -> Vec<AgentEvent> {
        if let Some(usage) = message.get("usage") {
            self.last_turn_input = Some(
                field(usage, "input_tokens")
                    + field(usage, "cache_creation_input_tokens")
                    + field(usage, "cache_read_input_tokens"),
            );
        }
        let mut events = vec![];
        for block in message["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                // The full text repeats what the deltas already delivered.
                Some("text") if !self.delta_emitted => {
                    let text = block["text"].as_str().unwrap_or_default();
                    if !text.is_empty() {
                        events.push(AgentEvent::Text {
                            text: text.to_string(),
                        });
                    }
                }
                Some("tool_use") => events.push(AgentEvent::ToolUse {
                    name: block["name"].as_str().unwrap_or("tool").to_string(),
                    input: tool_preview(
                        block["name"].as_str().unwrap_or_default(),
                        &block["input"],
                    ),
                }),
                // Thinking arrives again with the completed message, so only the block is
                // shown; thinking deltas stream the same text early. Redacted thinking
                // is never shown.
                Some("thinking") => {
                    let text = block["thinking"].as_str().unwrap_or_default();
                    if !text.is_empty() {
                        events.push(AgentEvent::Thinking {
                            text: text.to_string(),
                        });
                    }
                }
                // Redacted-thinking blocks are never shown.
                _ => {}
            }
        }
        events
    }

    fn result(&mut self, value: &Value) -> Vec<AgentEvent> {
        let text = value["result"].as_str().unwrap_or_default().to_string();
        let subtype = value["subtype"].as_str().unwrap_or_default();
        let failed = value["is_error"].as_bool().unwrap_or(false)
            || subtype.contains("error")
            || subtype.contains("fail");
        let mut events = vec![if failed {
            AgentEvent::Stderr { text }
        } else {
            AgentEvent::Result { text }
        }];
        events.push(AgentEvent::Usage(self.usage(value)));
        events
    }

    fn usage(&self, value: &Value) -> Usage {
        let usage = &value["usage"];
        let cache_creation = field(usage, "cache_creation_input_tokens");
        let cache_read = field(usage, "cache_read_input_tokens");
        let cumulative = field(usage, "input_tokens") + cache_creation + cache_read;
        let context_window = value["modelUsage"]
            .as_object()
            .and_then(|models| models.values().next())
            .and_then(|m| m["contextWindow"].as_u64())
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        Usage {
            input_tokens: self.last_turn_input.unwrap_or(cumulative),
            output_tokens: field(usage, "output_tokens"),
            context_window,
            cache_creation_tokens: cache_creation,
            cache_read_tokens: cache_read,
        }
    }
}

fn user(value: &Value) -> Vec<AgentEvent> {
    let stderr = value["tool_use_result"]["stderr"]
        .as_str()
        .unwrap_or_default();
    let mut events = vec![];
    for block in value["message"]["content"].as_array().into_iter().flatten() {
        if block["type"] != "tool_result" {
            continue;
        }
        let text = tool_result_text(&block["content"]);
        let failed = block["is_error"].as_bool().unwrap_or(false);
        events.push(if failed {
            AgentEvent::Stderr {
                text: preview(&text, cli::RESULT_PREVIEW),
            }
        } else if !stderr.is_empty() {
            AgentEvent::Stderr {
                text: preview(stderr, cli::RESULT_PREVIEW),
            }
        } else {
            AgentEvent::ToolResult {
                output: preview(&text, cli::RESULT_PREVIEW),
            }
        });
    }
    events
}

fn system(value: &Value) -> Vec<AgentEvent> {
    let subtype = value["subtype"].as_str().unwrap_or_default();
    let stderr = value["stderr"].as_str().unwrap_or_default();
    let hook_failed = value["outcome"].as_str().is_some_and(|o| o != "success");
    if !stderr.is_empty() {
        vec![AgentEvent::Stderr {
            text: stderr.to_string(),
        }]
    } else if subtype == "error" || hook_failed {
        vec![AgentEvent::Stderr {
            text: error_text(value),
        }]
    } else {
        vec![]
    }
}

fn error_text(value: &Value) -> String {
    cli::first_non_empty(value, &["message", "error", "output", "subtype"])
        .unwrap_or("unknown error")
        .to_string()
}

fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn tool_preview(name: &str, input: &Value) -> String {
    let keys: &[&str] = match name {
        "Read" | "Write" | "Edit" => &["file_path"],
        "Bash" => &["command"],
        "Glob" | "Grep" => &["pattern"],
        _ => &[
            "file_path",
            "path",
            "command",
            "pattern",
            "url",
            "query",
            "description",
            "prompt",
        ],
    };
    cli::preview_input(input, keys)
}

fn field(usage: &Value, key: &str) -> u64 {
    usage[key].as_u64().unwrap_or(0)
}

/// Non-JSON output: leaked API fragments are dropped, anything else surfaces as stderr.
fn non_json(line: &str) -> Vec<AgentEvent> {
    let text = strip_escapes(line);
    let text = text.trim();
    let base64_run = text.len() >= 60
        && !text.contains(' ')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    let leaked = text.contains("\"input_tokens\"") || text.contains("\"session_id\"");
    if text.is_empty() || base64_run || leaked {
        vec![]
    } else {
        vec![AgentEvent::Stderr {
            text: text.to_string(),
        }]
    }
}

/// Removes ANSI escape sequences (CSI and OSC) from a line.
fn strip_escapes(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(n) = chars.next() {
                    if n == '\u{7}' || (n == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(parser: &mut StreamParser, line: &str) -> Vec<AgentEvent> {
        parser.parse(line).events
    }

    #[test]
    fn deltas_suppress_the_repeated_full_text_but_not_tool_use() {
        let mut p = StreamParser::default();
        events(
            &mut p,
            r#"{"type":"stream_event","event":{"type":"message_start"}}"#,
        );
        let delta = events(
            &mut p,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}}"#,
        );
        assert_eq!(delta, [AgentEvent::TextDelta { text: "hi".into() }]);
        let full = events(
            &mut p,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
        );
        assert_eq!(
            full,
            [AgentEvent::ToolUse {
                name: "Bash".into(),
                input: "ls".into()
            }]
        );
    }

    #[test]
    fn full_text_is_emitted_when_no_delta_arrived() {
        let mut p = StreamParser::default();
        let out = events(
            &mut p,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"done"}]}}"#,
        );
        assert_eq!(
            out,
            [
                AgentEvent::Thinking { text: "x".into() },
                AgentEvent::Text {
                    text: "done".into()
                }
            ]
        );
    }

    #[test]
    fn thinking_deltas_are_not_shown_and_redacted_thinking_stays_hidden() {
        let mut p = StreamParser::default();
        events(
            &mut p,
            r#"{"type":"stream_event","event":{"type":"message_start"}}"#,
        );
        assert!(events(
            &mut p,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"par"}}}"#
        )
        .is_empty());
        let out = events(
            &mut p,
            r#"{"type":"assistant","message":{"content":[{"type":"redacted_thinking","data":"aaa"},{"type":"thinking","thinking":"tial"},{"type":"text","text":"ok"}]}}"#,
        );
        assert_eq!(
            out,
            [
                AgentEvent::Thinking {
                    text: "tial".into()
                },
                AgentEvent::Text { text: "ok".into() }
            ]
        );
    }

    #[test]
    fn tool_results_and_errors() {
        let mut p = StreamParser::default();
        let ok = events(
            &mut p,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"fine"}]}}"#,
        );
        assert_eq!(
            ok,
            [AgentEvent::ToolResult {
                output: "fine".into()
            }]
        );
        let bad = events(
            &mut p,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"boom"}],"is_error":true}]}}"#,
        );
        assert_eq!(
            bad,
            [AgentEvent::Stderr {
                text: "boom".into()
            }]
        );
    }

    #[test]
    fn result_is_followed_by_usage_with_last_turn_input() {
        let mut p = StreamParser::default();
        events(
            &mut p,
            r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":2,"cache_creation_input_tokens":10,"cache_read_input_tokens":100}}}"#,
        );
        let out = events(
            &mut p,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","usage":{"input_tokens":9,"output_tokens":7,"cache_creation_input_tokens":20,"cache_read_input_tokens":200},"modelUsage":{"m":{"contextWindow":1000}}}"#,
        );
        assert_eq!(out[0], AgentEvent::Result { text: "ok".into() });
        assert_eq!(
            out[1],
            AgentEvent::Usage(Usage {
                input_tokens: 112,
                output_tokens: 7,
                context_window: 1000,
                cache_creation_tokens: 20,
                cache_read_tokens: 200,
            })
        );
    }

    #[test]
    fn failed_result_becomes_stderr() {
        let mut p = StreamParser::default();
        let out = events(
            &mut p,
            r#"{"type":"result","subtype":"error_during_execution","result":"nope"}"#,
        );
        assert_eq!(
            out[0],
            AgentEvent::Stderr {
                text: "nope".into()
            }
        );
        assert!(matches!(
            out[1],
            AgentEvent::Usage(Usage {
                context_window: 200_000,
                ..
            })
        ));
    }

    #[test]
    fn system_and_rate_limit_events() {
        let mut p = StreamParser::default();
        assert!(events(&mut p, r#"{"type":"system","subtype":"status"}"#).is_empty());
        assert!(
            events(
                &mut p,
                r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#
            )
            .is_empty()
        );
        assert!(!crate::process::LineParser::rate_limited(&p));
        assert_eq!(
            events(
                &mut p,
                r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#
            ),
            [AgentEvent::Text {
                text: "[rate limited] rejected".into()
            }]
        );
        assert!(crate::process::LineParser::rate_limited(&p));
        assert_eq!(
            events(
                &mut p,
                r#"{"type":"system","subtype":"hook_response","outcome":"error","output":"bad hook"}"#
            ),
            [AgentEvent::Stderr {
                text: "bad hook".into()
            }]
        );
        assert_eq!(
            events(&mut p, r#"{"type":"mystery"}"#),
            [AgentEvent::Text {
                text: "[mystery]".into()
            }]
        );
    }

    #[test]
    fn non_json_lines() {
        let mut p = StreamParser::default();
        let parsed = p.parse("\u{1b}[31mwarning: x\u{1b}[0m");
        assert!(!parsed.json);
        assert_eq!(
            parsed.events,
            [AgentEvent::Stderr {
                text: "warning: x".into()
            }]
        );
        let wrapped = p.parse("\u{1b}[0m{\"type\":\"system\",\"subtype\":\"status\"}");
        assert!(wrapped.json && wrapped.events.is_empty());
        assert!(p.parse(&"A".repeat(80)).events.is_empty());
        assert!(p.parse("{\"input_tokens\": 3, broken").events.is_empty());
    }

    #[test]
    fn tool_previews_are_per_tool_and_truncated() {
        let long = "x".repeat(300);
        let input: Value = serde_json::json!({ "file_path": "/a/b", "command": long });
        assert_eq!(tool_preview("Read", &input), "/a/b");
        assert_eq!(tool_preview("Bash", &input).chars().count(), 120);
        assert_eq!(tool_preview("Other", &serde_json::json!({"url": "u"})), "u");
    }

    #[test]
    fn args_include_model_system_prompt_and_tools_only_when_set() {
        let (mut request, _rx) = crate::test_util::request("p", None, &[]);
        assert!(!build_args(&request).contains(&"--model".to_string()));
        request.model = Some("opus".into());
        request.system_prompt = Some("s".into());
        request.allowed_tools = vec![Tool::Read, Tool::Bash];
        let args = build_args(&request).join(" ");
        assert!(args.contains("--model opus"));
        assert!(args.contains("--append-system-prompt s"));
        assert!(args.contains("--allowedTools Read,Bash"));
        request.allowed_tools = crate::PLANNER_TOOLS.to_vec();
        let args = build_args(&request).join(" ");
        assert!(args.contains("--allowedTools Read,Glob,Grep,Edit,Write"));
    }

    #[test]
    fn role_tools_map_to_their_claude_names() {
        assert_eq!(claude_tools(Tool::Bash), ["Bash"]);
        assert_eq!(claude_tools(Tool::Edit), ["Edit"]);
        assert_eq!(claude_tools(Tool::Write), ["Write"]);
        assert_eq!(claude_tools(Tool::Read), ["Read"]);
        assert_eq!(claude_tools(Tool::Glob), ["Glob"]);
        assert_eq!(claude_tools(Tool::Grep), ["Grep"]);
        assert_eq!(claude_tools(Tool::NotebookEdit), ["NotebookEdit"]);
        assert_eq!(claude_tools(Tool::WebFetch), ["WebFetch"]);
        assert_eq!(claude_tools(Tool::WebSearch), ["WebSearch"]);
    }

    const ANSWER: &str =
        r#"read -r line && [ "$line" = "$REQUEST" ] && printf '%s\n' "$OUTPUT"; sleep 30"#;

    fn fake_claude(script: &'static str, output: &'static str) -> ProcessSpec {
        ProcessSpec {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![("REQUEST", INITIALIZE_REQUEST), ("OUTPUT", output)],
            ..discovery_spec()
        }
    }

    fn ids(models: &[DiscoveredModel]) -> Vec<&str> {
        models.iter().map(|m| m.id.as_str()).collect()
    }

    #[tokio::test]
    async fn discovery_reads_the_models_from_the_initialize_response() {
        let spec = fake_claude(
            ANSWER,
            include_str!("../tests/fixtures/claude-initialize.jsonl"),
        );
        let models = tokio::time::timeout(std::time::Duration::from_secs(5), discover_with(spec))
            .await
            .expect("the process was killed after the answer")
            .unwrap();
        assert_eq!(ids(&models), ["default", "opus", "sonnet[1m]", "haiku"]);
        assert!(models.iter().all(|m| m.listed));
    }

    #[tokio::test]
    async fn discovery_fails_when_the_harness_exits_without_answering() {
        let spec = fake_claude(r#"read -r line; echo '{"type":"system"}'"#, "");
        match discover_with(spec).await {
            Err(ProviderError::Discovery { provider, .. }) => assert_eq!(provider, "claude"),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn discovery_reports_an_error_response() {
        let spec = fake_claude(
            ANSWER,
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"r1","error":"not logged in"}}"#,
        );
        match discover_with(spec).await {
            Err(ProviderError::Discovery { reason, .. }) => {
                assert!(reason.contains("not logged in"), "{reason}");
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn discovery_of_a_missing_cli_is_cli_not_found() {
        let spec = ProcessSpec {
            program: "patok-no-such-cli".into(),
            ..discovery_spec()
        };
        let err = discover_with(spec).await.unwrap_err();
        assert!(matches!(err, ProviderError::CliNotFound { .. }), "{err}");
    }

    #[test]
    fn the_discovery_spec_is_the_spec_invocation() {
        let spec = discovery_spec();
        assert_eq!(
            spec.args.join(" "),
            "-p --input-format stream-json --output-format stream-json --verbose --no-session-persistence"
        );
        assert_eq!(spec.stdin, format!("{INITIALIZE_REQUEST}\n"));
        assert_eq!(spec.env, [("CLAUDECODE", "")]);
    }

    #[test]
    fn initialize_lines_that_are_not_the_answer_are_ignored() {
        assert!(parse_initialize_line("not json").is_none());
        assert!(parse_initialize_line(r#"{"type":"system"}"#).is_none());
        assert!(
            parse_initialize_line(
                r#"{"type":"control_response","response":{"request_id":"r9","response":{"models":["a"]}}}"#
            )
            .is_none()
        );
    }

    #[test]
    fn a_missing_or_empty_models_list_is_an_error() {
        for line in [
            r#"{"type":"control_response","response":{"request_id":"r1","response":{"models":"x"}}}"#,
            r#"{"type":"control_response","response":{"request_id":"r1","response":{"models":[]}}}"#,
        ] {
            assert!(
                matches!(parse_initialize_line(line), Some(Err(_))),
                "{line}"
            );
        }
    }

    #[test]
    fn model_entries_may_be_objects_and_duplicates_are_dropped() {
        let line = r#"{"type":"control_response","response":{"request_id":"r1","response":{"models":[{"value":"opus","displayName":"Opus"},"opus","","sonnet",3]}}}"#;
        let models = parse_initialize_line(line).unwrap().unwrap();
        assert_eq!(
            models,
            [
                DiscoveredModel {
                    id: "opus".into(),
                    display_name: Some("Opus".into()),
                    listed: true
                },
                DiscoveredModel {
                    id: "sonnet".into(),
                    display_name: None,
                    listed: true
                },
            ]
        );
    }

    #[test]
    fn models_outside_the_initialize_payload_are_not_read() {
        let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"r1","models":["opus"]}}"#;
        assert!(matches!(parse_initialize_line(line), Some(Err(_))));
    }
}
