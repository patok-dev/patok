//! JSON schema generation for the two config schemas (schemars). The generated schemas are
//! committed as `schemas/daemon.schema.json` and `schemas/tui.schema.json` at the repo
//! root; a unit test regenerates both and asserts they match, so they cannot drift.
//! Parsing stays manual (per-field fallback), so the schemas
//! are documentation only.

/// The daemon schema as pretty-printed JSON.
pub fn daemon_json_schema() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(super::daemon::DaemonSettings))
        .expect("the daemon schema serializes")
}

/// The tui schema as pretty-printed JSON.
pub fn tui_json_schema() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(super::tui::TuiSettings))
        .expect("the tui schema serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed schemas must match what schemars generates for the current
    /// settings structs, so they cannot drift.
    #[test]
    fn committed_schemas_are_fresh() {
        for (file, generated) in [
            ("daemon.schema.json", daemon_json_schema()),
            ("tui.schema.json", tui_json_schema()),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../schemas")
                .join(file);
            let committed = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            assert_eq!(
                committed,
                generated,
                "{} is stale; regenerate it from the config schemas",
                path.display()
            );
        }
    }

    #[test]
    fn schemas_parse_and_list_representative_properties() {
        for (schema, properties) in [
            (
                daemon_json_schema(),
                [
                    "provider",
                    "model",
                    "stages",
                    "agent_timeout_secs",
                    "embedding_port",
                ]
                .as_slice(),
            ),
            (
                tui_json_schema(),
                [
                    "theme",
                    "truecolor",
                    "preview_wrap",
                    "agent_pane_split",
                    "update_channel",
                    "rail_mode",
                ]
                .as_slice(),
            ),
        ] {
            let json: serde_json::Value = serde_json::from_str(&schema).unwrap();
            for property in properties {
                assert!(
                    json["properties"][property].is_object(),
                    "{schema:?} lacks {property}"
                );
            }
        }
    }
}
