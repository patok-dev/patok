//! The Codex discovery parser against a hand-made `codex debug models` output.
use patok_providers::DiscoveredModel;
use patok_providers::codex::parse_models_line;

#[test]
fn the_debug_models_output_lists_every_model() {
    let fixture = include_str!("fixtures/codex-debug-models.jsonl");
    let lines: Vec<&str> = fixture.lines().collect();
    assert_eq!(lines.len(), 1);
    let models = parse_models_line(lines[0])
        .expect("an answer")
        .expect("models");
    let model = |id: &str, name: Option<&str>, listed: bool| DiscoveredModel {
        id: id.into(),
        display_name: name.map(String::from),
        listed,
    };
    let expected = vec![
        model("gpt-test-pro", Some("GPT-Test-Pro"), true),
        model("gpt-test-mini", Some("GPT-Test-Mini"), true),
        model("gpt-test-reserve", Some("GPT-Test-Reserve"), false),
        model("gpt-test-plain", None, false),
    ];
    assert_eq!(models, expected);
    assert!(models.iter().all(|m| !m.id.contains("example")
        && !m.display_name.as_deref().unwrap_or("").contains("example")));
}
