//! The Mistral provider: drives the `vibe` CLI in programmatic mode and normalises its
//! newline-delimited JSON stream (`--output streaming`).

use async_trait::async_trait;
use patok_core::event::{AgentEvent, preview};
use serde_json::Value;

use crate::process::{self, LineParser, Parsed, ProcessSpec};
use crate::{Provider, ProviderError, SessionRequest, SessionResult, cli_in};

const TOOL_PREVIEW: usize = 120;
const RESULT_PREVIEW: usize = 200;

/// Name of the CLI binary this provider needs.
pub const CLI: &str = "vibe";

pub struct Vibe;

impl Vibe {
    /// Fails with a clear error naming the CLI when `vibe` is not on `PATH`. Run before a
    /// session starts.
    pub fn check_available() -> Result<(), ProviderError> {
        Self::check_available_in(std::env::var_os("PATH").as_deref())
    }

    /// The availability check against an explicit `PATH` value.
    pub fn check_available_in(path: Option<&std::ffi::OsStr>) -> Result<(), ProviderError> {
        if cli_in(CLI, path) {
            Ok(())
        } else {
            Err(ProviderError::CliNotFound { cli: CLI.into() })
        }
    }
}

#[async_trait]
impl Provider for Vibe {
    fn slug(&self) -> &'static str {
        "mistral"
    }

    async fn run_session(&self, request: SessionRequest) -> Result<SessionResult, ProviderError> {
        Self::check_available()?;
        let spec = ProcessSpec {
            program: CLI.into(),
            args: build_args(&request),
            env: vec![],
            stdin: String::new(),
        };
        let model = request.model.clone().unwrap_or_default();
        process::run(spec, &request, StreamParser, self.slug(), &model).await
    }
}

/// Vibe has no system-prompt flag and takes the prompt as the `-p` argument, so the system
/// prompt is prepended to it.
fn build_args(request: &SessionRequest) -> Vec<String> {
    let prompt = match request.system_prompt.as_deref().filter(|s| !s.is_empty()) {
        Some(system) => format!("{system}\n\n{}", request.prompt),
        None => request.prompt.clone(),
    };
    let mut args: Vec<String> = vec![
        "-p".into(),
        prompt,
        "--output".into(),
        "streaming".into(),
        "--trust".into(),
        // No human answers approval prompts; the tool allowlist is the safety boundary.
        "--auto-approve".into(),
    ];
    // With `-p`, `--enabled-tools` disables every tool not listed.
    for tool in vibe_tools(&request.allowed_tools) {
        args.extend(["--enabled-tools".into(), tool.into()]);
    }
    args
}

/// Maps role allowlist names (Claude tool names) to Vibe's built-in tool names.
fn vibe_tools(allowed: &[String]) -> Vec<&'static str> {
    let mut tools: Vec<&'static str> = vec![];
    for name in allowed {
        let mapped: &[&'static str] = match name.as_str() {
            "Bash" => &["bash"],
            "Edit" => &["edit", "search_replace"],
            "Write" => &["write_file"],
            "Read" => &["read_file"],
            "Glob" | "Grep" => &["grep"],
            "WebFetch" => &["web_fetch"],
            "WebSearch" => &["web_search"],
            // NotebookEdit and unknown names have no Vibe equivalent.
            _ => &[],
        };
        for tool in mapped {
            if !tools.contains(tool) {
                tools.push(tool);
            }
        }
    }
    tools
}

/// Translates Vibe's streaming JSON entries into normalised events.
#[derive(Default)]
pub struct StreamParser;

impl LineParser for StreamParser {
    fn parse_line(&mut self, line: &str) -> Parsed {
        self.parse(line)
    }
}

impl StreamParser {
    pub fn parse(&mut self, line: &str) -> Parsed {
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
}

fn events_for(value: &Value) -> Vec<AgentEvent> {
    match value["type"].as_str().unwrap_or_default() {
        "message" if value["role"] == "assistant" => {
            let text = content_text(&value["content"]);
            if text.is_empty() {
                vec![]
            } else {
                vec![AgentEvent::Text { text }]
            }
        }
        "effect" => effect(value),
        "error" => vec![AgentEvent::Stderr {
            text: ["message", "error", "text"]
                .iter()
                .find_map(|k| value[*k].as_str().filter(|s| !s.is_empty()))
                .unwrap_or("unknown error")
                .to_string(),
        }],
        // The agent's reasoning, normalised into the same thinking event the Claude
        // provider emits.
        "reasoning" => {
            let text = value["text"].as_str().unwrap_or_default();
            if text.is_empty() {
                vec![]
            } else {
                vec![AgentEvent::Thinking {
                    text: text.to_string(),
                }]
            }
        }
        // User echo and anything unknown are not shown.
        _ => vec![],
    }
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// A tool call entry: the call itself plus, once settled, its result.
fn effect(value: &Value) -> Vec<AgentEvent> {
    let status = value["state"]["status"].as_str().unwrap_or("completed");
    if matches!(status, "pending" | "running" | "in_progress" | "streaming") {
        return vec![];
    }
    let name = value["detail"]["toolName"]
        .as_str()
        .or(value["title"].as_str())
        .unwrap_or("tool");
    let input = tool_preview(&value["detail"]["input"]);
    let state = &value["state"];
    let output = state["outputText"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| state["display"]["message"].as_str().map(str::to_string))
        .unwrap_or_default();
    let result = if status == "completed" {
        AgentEvent::ToolResult {
            output: preview(&output, RESULT_PREVIEW),
        }
    } else {
        AgentEvent::Stderr {
            text: preview(
                &if output.is_empty() {
                    format!("{name} {status}")
                } else {
                    output
                },
                RESULT_PREVIEW,
            ),
        }
    };
    vec![
        AgentEvent::ToolUse {
            name: name.to_string(),
            input,
        },
        result,
    ]
}

fn tool_preview(input: &Value) -> String {
    let text = [
        "filePath",
        "file_path",
        "path",
        "command",
        "pattern",
        "url",
        "query",
    ]
    .iter()
    .find_map(|k| input[*k].as_str())
    .map_or_else(|| input.to_string(), str::to_string);
    preview(&text, TOOL_PREVIEW)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn all_events(transcript: &str) -> Vec<AgentEvent> {
        let mut parser = StreamParser;
        transcript
            .lines()
            .flat_map(|line| parser.parse(line).events)
            .collect()
    }

    #[test]
    fn recorded_write_file_session_normalises() {
        let events = all_events(include_str!("../tests/fixtures/vibe-write-file.jsonl"));
        let [
            AgentEvent::Thinking { .. },
            AgentEvent::ToolUse { name, input },
            AgentEvent::ToolResult { .. },
            AgentEvent::Thinking { .. },
            AgentEvent::Text { text },
        ] = events.as_slice()
        else {
            panic!("unexpected events: {events:#?}");
        };
        assert_eq!(name, "write_file");
        assert!(input.ends_with("hello.txt"), "{input}");
        assert_eq!(text, "done");
    }

    #[test]
    fn failed_tool_becomes_stderr_and_successful_one_a_result() {
        let events = all_events(include_str!("../tests/fixtures/vibe-failed-tool.jsonl"));
        assert_eq!(
            events,
            [
                AgentEvent::Thinking {
                    text: "I will run ls.".into()
                },
                AgentEvent::ToolUse {
                    name: "bash".into(),
                    input: "ls /nope".into()
                },
                AgentEvent::Stderr {
                    text: "ls: cannot access '/nope': No such file or directory".into()
                },
                AgentEvent::ToolUse {
                    name: "grep".into(),
                    input: "fn main".into()
                },
                AgentEvent::ToolResult {
                    output: "src/main.rs:1:fn main() {}".into()
                },
                AgentEvent::Text {
                    text: "The directory does not exist.".into()
                },
            ]
        );
    }

    #[test]
    fn recorded_reasoning_normalises_to_the_same_markdown_thinking_event() {
        let events = all_events(include_str!("../tests/fixtures/vibe-thinking.jsonl"));
        assert_eq!(
            events,
            [
                // The user echo is not shown.
                AgentEvent::Thinking {
                    text: "## Plan\nRead the **task parser**, then:\n- parse the *task lines*\n- fix the `task.rs` fixture\n- run cargo test\nFinally:\n```rust\nlet tasks = parse(&text);\n```\n"
                        .into()
                },
                AgentEvent::Text {
                    text: "done".into()
                },
            ]
        );
    }

    #[test]
    fn non_json_error_and_unknown_lines() {
        let mut p = StreamParser;
        let plain = p.parse("Error: no API key");
        assert!(!plain.json);
        assert_eq!(
            plain.events,
            [AgentEvent::Stderr {
                text: "Error: no API key".into()
            }]
        );
        assert!(p.parse("").events.is_empty());
        let unknown = p.parse(r#"{"type":"mystery"}"#);
        assert!(unknown.json && unknown.events.is_empty());
        assert_eq!(
            p.parse(r#"{"type":"error","message":"boom"}"#).events,
            [AgentEvent::Stderr {
                text: "boom".into()
            }]
        );
        // A tool that is still running has not settled yet.
        assert!(
            p.parse(r#"{"type":"effect","title":"bash","state":{"status":"running"}}"#)
                .events
                .is_empty()
        );
    }

    #[test]
    fn args_map_role_tools_and_prepend_the_system_prompt() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut request = SessionRequest {
            model: None,
            prompt: "do it".into(),
            system_prompt: Some("sys".into()),
            project_dir: ".".into(),
            events: tx,
            log_dir: ".".into(),
            label: "t".into(),
            idle_timeout: std::time::Duration::from_secs(1),
            cancel: Default::default(),
            allowed_tools: crate::PLANNER_TOOLS
                .iter()
                .map(ToString::to_string)
                .collect(),
        };
        let args = build_args(&request);
        assert_eq!(args[..2], ["-p", "sys\n\ndo it"]);
        let tools: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "--enabled-tools")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(
            tools,
            ["read_file", "grep", "edit", "search_replace", "write_file"]
        );
        request.allowed_tools = crate::BUILDER_TOOLS
            .iter()
            .map(ToString::to_string)
            .collect();
        let args = build_args(&request).join(" ");
        assert!(args.contains("--enabled-tools bash"));
        assert!(args.contains("--enabled-tools web_search"));
        assert!(!args.contains("Notebook"));
    }

    #[test]
    fn availability_check_uses_a_fake_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();

        let missing = Vibe::check_available_in(Some(&path)).unwrap_err();
        assert!(matches!(&missing, ProviderError::CliNotFound { cli } if cli == "vibe"));
        assert!(missing.to_string().contains("`vibe`"), "{missing}");
        assert!(Vibe::check_available_in(None).is_err());

        let bin = dir.path().join("vibe");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        // Present but not executable still counts as missing.
        assert!(Vibe::check_available_in(Some(&path)).is_err());
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Vibe::check_available_in(Some(&path)).unwrap();
    }
}
