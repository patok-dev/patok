//! The Claude parser against a recorded CLI transcript.

use patok_core::event::AgentEvent;
use patok_providers::claude::StreamParser;

#[test]
fn recorded_write_file_session_normalises() {
    let transcript = include_str!("fixtures/claude-write-file.jsonl");
    let mut parser = StreamParser::default();
    let events: Vec<AgentEvent> = transcript
        .lines()
        .flat_map(|line| parser.parse(line).events)
        .collect();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolUse { name, .. } if name == "Write")),
        "{events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { .. }))
    );
    let streamed: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(streamed.contains("hello.txt"), "{streamed}");
    // Deltas were seen, so the completed message must not repeat the text.
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Text { .. })));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Stderr { .. }))
    );
    let [.., AgentEvent::Result { text }, AgentEvent::Usage(usage)] = events.as_slice() else {
        panic!("transcript must end with Result then Usage: {events:#?}");
    };
    assert!(text.contains("hello.txt"));
    assert_eq!(usage.context_window, 1_000_000);
    assert!(usage.output_tokens > 0 && usage.input_tokens > 0);
}

/// The markdown thinking both recorded thinking fixtures carry, byte for byte.
const THINKING_MARKDOWN: &str = "## Plan\nRead the **task parser**, then:\n- parse the *task lines*\n- fix the `task.rs` fixture\n- run cargo test\nFinally:\n```rust\nlet tasks = parse(&text);\n```\n";

#[test]
fn recorded_thinking_session_normalises_to_one_markdown_thinking_event() {
    let transcript = include_str!("fixtures/claude-thinking.jsonl");
    let mut parser = StreamParser::default();
    let events: Vec<AgentEvent> = transcript
        .lines()
        .flat_map(|line| parser.parse(line).events)
        .collect();

    // The streamed thinking deltas are not shown; the completed block is, once.
    let thinking: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Thinking { .. }))
        .collect();
    assert_eq!(thinking.len(), 1, "{events:#?}");
    assert_eq!(
        thinking[0],
        &AgentEvent::Thinking {
            text: THINKING_MARKDOWN.to_string()
        }
    );
    // Redacted thinking stays hidden and the turn still completes normally.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Stderr { .. } | AgentEvent::TextDelta { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Result { .. }))
    );
}
