//! The Claude provider: drives the `claude` CLI in print mode and normalises its
//! stream-json output.

use async_trait::async_trait;
use patok_core::event::{AgentEvent, Usage, preview};
use serde_json::Value;

use crate::process::{self, LineParser, Parsed, ProcessSpec};
use crate::{Provider, ProviderError, SessionRequest, SessionResult};

const TOOL_PREVIEW: usize = 120;
const RESULT_PREVIEW: usize = 200;
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

pub struct Claude;

#[async_trait]
impl Provider for Claude {
    fn slug(&self) -> &'static str {
        "claude"
    }

    async fn run_session(&self, request: SessionRequest) -> Result<SessionResult, ProviderError> {
        let spec = ProcessSpec {
            program: "claude".into(),
            args: build_args(&request),
            // Marks a nested Claude session; emptied so the child does not refuse to start.
            env: vec![("CLAUDECODE", "")],
            stdin: request.prompt.clone(),
        };
        let model = request.model.clone().unwrap_or_default();
        process::run(spec, &request, StreamParser::default(), self.slug(), &model).await
    }
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
        args.extend(["--allowedTools".into(), request.allowed_tools.join(",")]);
    }
    args
}

/// Translates Claude's stream-json lines into normalised events. Stateful: text arrives twice
/// (partial deltas, then the completed message), and usage reports the last turn's input.
#[derive(Default)]
pub struct StreamParser {
    delta_emitted: bool,
    last_turn_input: Option<u64>,
}

impl LineParser for StreamParser {
    fn parse_line(&mut self, line: &str) -> Parsed {
        self.parse(line)
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
            "rate_limit_event" => rate_limit(value),
            other => vec![AgentEvent::Text {
                text: format!("[{other}]"),
            }],
        }
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
                text: preview(&text, RESULT_PREVIEW),
            }
        } else if !stderr.is_empty() {
            AgentEvent::Stderr {
                text: preview(stderr, RESULT_PREVIEW),
            }
        } else {
            AgentEvent::ToolResult {
                output: preview(&text, RESULT_PREVIEW),
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

fn rate_limit(value: &Value) -> Vec<AgentEvent> {
    let status = value["rate_limit_info"]["status"]
        .as_str()
        .unwrap_or("unknown");
    if status == "allowed" {
        return vec![];
    }
    vec![AgentEvent::Text {
        text: format!("[rate limited] {status}"),
    }]
}

fn error_text(value: &Value) -> String {
    ["message", "error", "output", "subtype"]
        .iter()
        .find_map(|k| value[*k].as_str().filter(|s| !s.is_empty()))
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
    let text = keys
        .iter()
        .find_map(|k| input[*k].as_str())
        .map_or_else(|| input.to_string(), str::to_string);
    preview(&text, TOOL_PREVIEW)
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
        assert_eq!(
            events(
                &mut p,
                r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#
            ),
            [AgentEvent::Text {
                text: "[rate limited] rejected".into()
            }]
        );
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
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut request = SessionRequest {
            model: None,
            prompt: "p".into(),
            system_prompt: None,
            project_dir: ".".into(),
            events: tx,
            log_dir: ".".into(),
            label: "t".into(),
            idle_timeout: std::time::Duration::from_secs(1),
            cancel: Default::default(),
            allowed_tools: vec![],
        };
        assert!(!build_args(&request).contains(&"--model".to_string()));
        request.model = Some("opus".into());
        request.system_prompt = Some("s".into());
        request.allowed_tools = vec!["Read".into(), "Bash".into()];
        let args = build_args(&request).join(" ");
        assert!(args.contains("--model opus"));
        assert!(args.contains("--append-system-prompt s"));
        assert!(args.contains("--allowedTools Read,Bash"));
        request.allowed_tools = crate::PLANNER_TOOLS
            .iter()
            .map(ToString::to_string)
            .collect();
        let args = build_args(&request).join(" ");
        assert!(args.contains("--allowedTools Read,Glob,Grep,Edit,Write"));
    }
}
