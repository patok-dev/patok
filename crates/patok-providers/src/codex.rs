//! The Codex provider: drives the `codex` CLI in its non-interactive `exec` mode and
//! normalises its `--json` JSONL event stream.
//!
//! Codex has no system-prompt flag (like the Vibe provider, the system prompt is
//! prepended to the prompt) and no CLI tool allowlist: shell and file edits have no
//! toggle, so the workspace-write sandbox is their boundary; the one tool toggle it
//! has, built-in web search, follows the role allowlist through the `tools.web_search`
//! config override.

use async_trait::async_trait;
use patok_core::event::{AgentEvent, preview};
use serde_json::Value;

use crate::process::{self, LineParser, Parsed, ProcessSpec};
use crate::{Provider, ProviderError, SessionRequest, SessionResult, cli_in};

const TOOL_PREVIEW: usize = 120;
const RESULT_PREVIEW: usize = 200;

/// Name of the CLI binary this provider needs.
pub const CLI: &str = "codex";

pub struct Codex;

impl Codex {
    /// Fails with a clear error naming the CLI when `codex` is not on `PATH`. Run before a
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
impl Provider for Codex {
    fn slug(&self) -> &'static str {
        "codex"
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

/// Codex exec has no system-prompt flag and takes the prompt as a positional argument,
/// so the system prompt is prepended to it.
fn build_args(request: &SessionRequest) -> Vec<String> {
    let prompt = match request.system_prompt.as_deref().filter(|s| !s.is_empty()) {
        Some(system) => format!("{system}\n\n{}", request.prompt),
        None => request.prompt.clone(),
    };
    let mut args: Vec<String> = vec![
        "exec".into(),
        // The JSONL event stream.
        "--json".into(),
        // Full-auto: no human answers approval prompts. `exec` mode asks none and the
        // approval policy is pinned to never; the workspace-write sandbox is the safety
        // boundary for the mapped tool allowlist.
        "-s".into(),
        "workspace-write".into(),
        "-c".into(),
        "approval_policy=\"never\"".into(),
    ];
    if let Some(model) = request.model.as_deref().filter(|m| !m.is_empty()) {
        args.extend(["-m".into(), model.into()]);
    }
    // The allowlist's web-search tool, enabled or disabled per the role allowlist
    // (shell and apply_patch have no toggle; the sandbox is their boundary).
    let web = if codex_tools(&request.allowed_tools).contains(&"web_search") {
        "true"
    } else {
        "false"
    };
    args.extend(["-c".into(), format!("tools.web_search={web}")]);
    args.push(prompt);
    args
}

/// Maps role allowlist names (Claude tool names) to Codex's tool names: commands and
/// file reads/searches run through its shell tool, edits through apply_patch.
fn codex_tools(allowed: &[String]) -> Vec<&'static str> {
    let mut tools: Vec<&'static str> = vec![];
    for name in allowed {
        let mapped: &[&'static str] = match name.as_str() {
            "Bash" | "Read" | "Glob" | "Grep" => &["shell"],
            "Edit" | "Write" => &["apply_patch"],
            "WebSearch" => &["web_search"],
            // WebFetch, NotebookEdit and unknown names have no Codex equivalent.
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

/// Translates Codex's `--json` JSONL entries into normalised events.
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
        "item.started" => item_events(&value["item"], false),
        "item.completed" => item_events(&value["item"], true),
        "error" | "turn.failed" => vec![AgentEvent::Stderr {
            text: value["message"]
                .as_str()
                .or(value["error"]["message"].as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown error")
                .to_string(),
        }],
        // Thread and turn bookkeeping and anything unknown are not shown.
        _ => vec![],
    }
}

fn item_events(item: &Value, completed: bool) -> Vec<AgentEvent> {
    match item["type"].as_str().unwrap_or_default() {
        "agent_message" if completed => {
            let text = item["text"].as_str().unwrap_or_default().to_string();
            if text.is_empty() {
                vec![]
            } else {
                vec![AgentEvent::Text { text }]
            }
        }
        // The agent's reasoning, normalised into the same thinking event the Claude
        // provider emits.
        "reasoning" if completed => {
            let text = item["text"].as_str().unwrap_or_default().to_string();
            if text.is_empty() {
                vec![]
            } else {
                vec![AgentEvent::Thinking { text }]
            }
        }
        "command_execution" | "file_change" | "web_search" | "mcp_tool_call" => {
            tool_events(item, completed)
        }
        // Item updates stream partial output; the completed item carries it whole.
        _ => vec![],
    }
}

/// A tool item: the call itself once it starts, and once it settles its result -- a
/// failed tool is a stderr event, like the other providers.
fn tool_events(item: &Value, completed: bool) -> Vec<AgentEvent> {
    let name = tool_name(item);
    if !completed {
        return vec![AgentEvent::ToolUse {
            name,
            input: tool_input(item),
        }];
    }
    let status = item["status"].as_str().unwrap_or_default();
    if status == "completed" {
        vec![AgentEvent::ToolResult {
            output: preview(&tool_output(item), RESULT_PREVIEW),
        }]
    } else {
        let output = tool_output(item);
        vec![AgentEvent::Stderr {
            text: preview(
                &if output.is_empty() {
                    format!("{name} {status}")
                } else {
                    output
                },
                RESULT_PREVIEW,
            ),
        }]
    }
}

fn tool_name(item: &Value) -> String {
    match item["type"].as_str().unwrap_or_default() {
        "command_execution" => "shell".into(),
        "file_change" => "apply_patch".into(),
        "web_search" => "web_search".into(),
        other => item["tool"]
            .as_str()
            .or(item["name"].as_str())
            .unwrap_or(other)
            .to_string(),
    }
}

fn tool_input(item: &Value) -> String {
    let text = item["command"]
        .as_str()
        .map(str::to_string)
        .or_else(|| item["query"].as_str().map(str::to_string))
        .unwrap_or_else(|| change_summary(item).unwrap_or_else(|| item.to_string()));
    preview(&text, TOOL_PREVIEW)
}

fn tool_output(item: &Value) -> String {
    item["aggregated_output"]
        .as_str()
        .map(str::to_string)
        .or_else(|| change_summary(item))
        .unwrap_or_default()
}

/// The file changes of a `file_change` item, "kind path" per change.
fn change_summary(item: &Value) -> Option<String> {
    let changes = item["changes"].as_array()?;
    let summary = changes
        .iter()
        .map(|change| {
            let kind = change["kind"].as_str().unwrap_or("change");
            let path = change["path"].as_str().unwrap_or_default();
            format!("{kind} {path}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    (!summary.is_empty()).then_some(summary)
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
        let events = all_events(include_str!("../tests/fixtures/codex-write-file.jsonl"));
        let [
            AgentEvent::ToolUse { name, input },
            AgentEvent::ToolResult { .. },
            AgentEvent::Text { text },
        ] = events.as_slice()
        else {
            panic!("unexpected events: {events:#?}");
        };
        assert_eq!(name, "shell");
        assert!(input.contains("hello.txt"), "{input}");
        assert_eq!(text, "done");
    }

    #[test]
    fn failed_tool_becomes_stderr_and_the_message_stays_text() {
        let events = all_events(include_str!("../tests/fixtures/codex-failed-tool.jsonl"));
        assert_eq!(
            events,
            [
                AgentEvent::Text {
                    text: "I’ll run the requested command once and report its result.".into()
                },
                AgentEvent::ToolUse {
                    name: "shell".into(),
                    input: "/usr/bin/zsh -lc 'ls /nope'".into()
                },
                AgentEvent::Stderr {
                    text: "ls: cannot access '/nope': No such file or directory".into()
                },
                AgentEvent::Text {
                    text: "`ls /nope` failed: No such file or directory (exit code 2).".into()
                },
            ]
        );
    }

    #[test]
    fn recorded_reasoning_normalises_to_the_same_thinking_event() {
        // Recorded with `model_reasoning_summary="detailed"`: the reasoning item only
        // surfaces in the stream when the model's reasoning summary is on.
        let events = all_events(include_str!("../tests/fixtures/codex-reasoning.jsonl"));
        assert_eq!(
            events,
            [
                AgentEvent::Thinking {
                    text: "**Planning pointer reversal**".into()
                },
                AgentEvent::Text {
                    text: "Walk the list once, redirecting each node’s `next` pointer to the previous node; this takes O(n) time and O(1) extra space.".into()
                },
            ]
        );
    }

    #[test]
    fn recorded_file_change_normalises_to_apply_patch() {
        let events = all_events(include_str!("../tests/fixtures/codex-file-change.jsonl"));
        let [
            AgentEvent::Text { .. },
            AgentEvent::ToolUse { .. },
            AgentEvent::Stderr { .. },
            AgentEvent::ToolUse { name, input },
            AgentEvent::ToolResult { output },
            AgentEvent::Text { .. },
        ] = events.as_slice()
        else {
            panic!("unexpected events: {events:#?}");
        };
        assert_eq!(name, "apply_patch");
        assert!(input.contains("add"), "{input}");
        assert!(input.ends_with("notes.txt"), "{input}");
        assert_eq!(output, input);
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
        let thread = p.parse(r#"{"type":"thread.started","thread_id":"t"}"#);
        assert!(thread.json && thread.events.is_empty());
        assert_eq!(
            p.parse(r#"{"type":"error","message":"boom"}"#).events,
            [AgentEvent::Stderr {
                text: "boom".into()
            }]
        );
        // An item update streams partial output; the completed item carries it whole.
        assert!(
            p.parse(r#"{"type":"item.updated","item":{"id":"i","type":"command_execution"}}"#)
                .events
                .is_empty()
        );
    }

    fn request(prompt: &str, system: Option<&str>, tools: &[&str]) -> SessionRequest {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        SessionRequest {
            model: None,
            prompt: prompt.into(),
            system_prompt: system.map(str::to_string),
            project_dir: ".".into(),
            events: tx,
            log_dir: ".".into(),
            label: "t".into(),
            idle_timeout: std::time::Duration::from_secs(1),
            cancel: Default::default(),
            allowed_tools: tools.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn args_start_exec_json_and_pin_full_auto() {
        let args = build_args(&request("do it", None, &[]));
        assert_eq!(args[0], "exec");
        assert!(args.contains(&"--json".to_string()));
        // No human answers approval prompts: never-ask policy, sandboxed workspace.
        assert!(args.contains(&"-s".to_string()));
        assert!(args.contains(&"workspace-write".to_string()));
        assert!(args.contains(&"approval_policy=\"never\"".to_string()));
        // A role without web search disables codex's built-in web search tool.
        assert!(args.contains(&"tools.web_search=false".to_string()));
        // The prompt is the final positional argument, bare without a system prompt.
        assert_eq!(args.last().unwrap(), "do it");
    }

    #[test]
    fn args_prepend_the_system_prompt_and_pass_the_model() {
        let mut request = request("do it", Some("sys"), &[]);
        request.model = Some("gpt-5.1-codex".into());
        let args = build_args(&request);
        assert_eq!(args.last().unwrap(), "sys\n\ndo it");
        let model = args.windows(2).find(|w| w[0] == "-m").unwrap();
        assert_eq!(model[1], "gpt-5.1-codex");
        // An empty model means the CLI's default.
        request.model = Some(String::new());
        assert!(!build_args(&request).contains(&"-m".to_string()));
    }

    #[test]
    fn args_map_role_tools_to_codex_names() {
        let planner = build_args(&request("p", None, crate::PLANNER_TOOLS));
        assert!(planner.contains(&"tools.web_search=false".to_string()));
        let research = build_args(&request("r", None, crate::RESEARCH_TOOLS));
        assert!(research.contains(&"tools.web_search=true".to_string()));
        // The mapping itself: Claude role names to codex tool names, unmapped dropped.
        assert_eq!(
            codex_tools(&["Read", "Glob", "Grep", "Edit", "Write"].map(ToString::to_string)),
            ["shell", "apply_patch"]
        );
        assert_eq!(
            codex_tools(
                &crate::BUILDER_TOOLS
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            ),
            ["shell", "apply_patch", "web_search"]
        );
        // WebFetch and NotebookEdit have no codex equivalent.
        assert_eq!(
            codex_tools(&["WebFetch", "NotebookEdit"].map(ToString::to_string)),
            [] as [&'static str; 0]
        );
    }

    #[test]
    fn availability_check_uses_a_fake_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();

        let missing = Codex::check_available_in(Some(&path)).unwrap_err();
        assert!(matches!(&missing, ProviderError::CliNotFound { cli } if cli == "codex"));
        assert!(missing.to_string().contains("`codex`"), "{missing}");
        assert!(Codex::check_available_in(None).is_err());

        let bin = dir.path().join("codex");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        // Present but not executable still counts as missing.
        assert!(Codex::check_available_in(Some(&path)).is_err());
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Codex::check_available_in(Some(&path)).unwrap();
    }
}
