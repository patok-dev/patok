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

    /// The stages property, the `Stage` definition and the `StageId` definition stay
    /// in their committed shape: the id enumerates the five accepted spellings
    /// (including `implement`, the build stage's pre-enum spelling), derived from
    /// the same enum the parser reads, and the rename of the config struct cannot
    /// leak into the schema.
    #[test]
    fn the_stages_schema_stays_committed_byte_for_byte() {
        let json: serde_json::Value = serde_json::from_str(&daemon_json_schema()).unwrap();
        assert_eq!(
            json["properties"]["stages"],
            serde_json::json!({
                "type": "array",
                "items": { "$ref": "#/$defs/Stage" }
            })
        );
        assert_eq!(
            json["$defs"]["Stage"],
            serde_json::json!({
                "description": "One pipeline stage: id, label, enabled flag and an optional\nprompt override. A stage absent from the list is treated as disabled by the consumer;\norder is display-only.",
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean" },
                    "id": { "$ref": "#/$defs/StageId" },
                    "label": { "type": "string" },
                    "prompt": { "type": ["string", "null"] }
                },
                "required": ["id", "label", "enabled"]
            })
        );
        assert_eq!(
            json["$defs"]["StageId"],
            serde_json::json!({
                "description": "A stage id as written in a `[[daemon.stages]]` entry: the four pipeline\nstage spellings plus `implement`, the build stage's pre-enum spelling.",
                "type": "string",
                "enum": ["research", "plan", "build", "implement", "review"]
            })
        );
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
