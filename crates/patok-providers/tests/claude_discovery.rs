//! The Claude discovery parser against a hand-made initialize exchange.
use patok_providers::DiscoveredModel;
use patok_providers::claude::parse_initialize_line;

#[test]
fn the_initialize_response_lists_every_model() {
    let fixture = include_str!("fixtures/claude-initialize.jsonl");
    let lines: Vec<&str> = fixture.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(parse_initialize_line(lines[0]).is_none());
    assert!(parse_initialize_line(lines[1]).is_none());
    let models = lines
        .iter()
        .find_map(|l| parse_initialize_line(l))
        .expect("an answer")
        .expect("models");
    let expected: Vec<DiscoveredModel> = ["default", "opus", "sonnet[1m]", "haiku"]
        .into_iter()
        .map(|id| DiscoveredModel {
            id: id.into(),
            display_name: None,
            listed: true,
        })
        .collect();
    assert_eq!(models, expected);
    assert!(models.iter().all(|m| !m.id.contains("example")
        && !m.display_name.as_deref().unwrap_or("").contains("example")));
}
