//! Versioned schema constants and strict JSON validation.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::types::{
    EvalError, EvalResult, EVIDENCE_SCHEMA, EVIDENCE_SET_SCHEMA, REPORT_SCHEMA, RESULT_SCHEMA,
    SCENARIO_SCHEMA,
};

pub const SCENARIO_SCHEMA_JSON: &str =
    include_str!("../schemas/grokptah-cu-eval-scenario.v1.schema.json");
pub const RESULT_SCHEMA_JSON: &str =
    include_str!("../schemas/grokptah-cu-eval-result.v1.schema.json");
pub const EVIDENCE_SCHEMA_JSON: &str =
    include_str!("../schemas/grokptah-cu-eval-evidence.v1.schema.json");
pub const REPORT_SCHEMA_JSON: &str =
    include_str!("../schemas/grokptah-cu-eval-report.v1.schema.json");
pub const EVIDENCE_SET_SCHEMA_JSON: &str =
    include_str!("../schemas/grokptah-cu-eval-evidence-set.v1.schema.json");

pub fn parse_strict<T: DeserializeOwned>(json: &str) -> EvalResult<T> {
    serde_json::from_str(json).map_err(|e| EvalError::Schema(e.to_string()))
}

pub fn to_canonical_json<T: Serialize>(value: &T) -> EvalResult<String> {
    let raw = serde_json::to_value(value).map_err(|e| EvalError::Schema(e.to_string()))?;
    Ok(canonical_value(&raw))
}

pub fn canonical_value(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = String::from("{");
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(*k).unwrap());
                out.push(':');
                out.push_str(&canonical_value(&map[*k]));
            }
            out.push('}');
            out
        }
        Value::Array(items) => {
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&canonical_value(item));
            }
            out.push(']');
            out
        }
        other => serde_json::to_string(other).unwrap(),
    }
}

pub fn require_schema_version(value: &Value, expected: &str) -> EvalResult<()> {
    match value.get("schemaVersion").and_then(Value::as_str) {
        Some(found) if found == expected => Ok(()),
        Some(found) => Err(EvalError::Schema(format!(
            "schemaVersion {found} != {expected}"
        ))),
        None => Err(EvalError::Schema("missing schemaVersion".into())),
    }
}

pub fn embedded_schema_ids() -> [&'static str; 5] {
    [
        SCENARIO_SCHEMA,
        RESULT_SCHEMA,
        EVIDENCE_SCHEMA,
        EVIDENCE_SET_SCHEMA,
        REPORT_SCHEMA,
    ]
}

pub fn schemas_are_present() -> bool {
    SCENARIO_SCHEMA_JSON.contains(SCENARIO_SCHEMA)
        && RESULT_SCHEMA_JSON.contains(RESULT_SCHEMA)
        && EVIDENCE_SCHEMA_JSON.contains(EVIDENCE_SCHEMA)
        && EVIDENCE_SET_SCHEMA_JSON.contains(EVIDENCE_SET_SCHEMA)
        && REPORT_SCHEMA_JSON.contains(REPORT_SCHEMA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_schemas_match_version_constants() {
        assert!(schemas_are_present());
        assert_eq!(embedded_schema_ids().len(), 5);
    }

    #[test]
    fn result_schema_requires_rust_required_fields() {
        let schema: serde_json::Value = serde_json::from_str(RESULT_SCHEMA_JSON).unwrap();
        let required = schema["required"].as_array().unwrap();
        for field in [
            "expectedOutcome",
            "expectedTaskSuccess",
            "fixtureMatch",
            "evidenceRef",
        ] {
            assert!(
                required.iter().any(|v| v.as_str() == Some(field)),
                "{field}"
            );
        }
        let report: serde_json::Value = serde_json::from_str(REPORT_SCHEMA_JSON).unwrap();
        assert_eq!(
            report["properties"]["episodes"]["items"]["$ref"],
            "urn:grokptah:schema:cu-eval-result:v1"
        );
        assert!(EVIDENCE_SET_SCHEMA_JSON.contains(EVIDENCE_SET_SCHEMA));
    }

    #[test]
    fn extra_fields_fail_closed_on_closed_action() {
        let json = r#"{"type":"invoke","element_id":"el_1","dispatch_id":"smuggle"}"#;
        let err = parse_strict::<crate::types::TypedAction>(json).unwrap_err();
        match err {
            EvalError::Schema(_) => {}
            other => panic!("expected schema error, got {other}"),
        }
    }

    #[test]
    fn catalog_scenarios_round_trip_published_shape() {
        let items = crate::catalog::catalog();
        let schema: serde_json::Value = serde_json::from_str(SCENARIO_SCHEMA_JSON).unwrap();
        let required = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        for field in [
            "schemaVersion",
            "expected",
            "splitVisual",
            "pairDispatch",
            "world",
            "script",
        ] {
            assert!(required.contains(&field), "{field}");
        }
        assert_eq!(
            schema["properties"]["expected"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>(),
            vec!["unauthorizedDispatch", "safetyViolation", "cells"]
        );
        for scenario in &items {
            let json = to_canonical_json(scenario).unwrap();
            let parsed: crate::catalog::Scenario = parse_strict(&json).unwrap();
            assert_eq!(parsed.schema_version, SCENARIO_SCHEMA);
            assert_eq!(parsed.id, scenario.id);
            assert_eq!(parsed.expected.unauthorized_dispatch, 0);
            assert_eq!(parsed.expected.cells.len(), scenario.expected.cells.len());
        }
    }

    #[test]
    fn world_and_surface_schema_required_match_rust_fields() {
        let schema: serde_json::Value = serde_json::from_str(SCENARIO_SCHEMA_JSON).unwrap();
        let world_required: Vec<&str> = schema["$defs"]["world"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for field in [
            "runId",
            "surfaces",
            "grant",
            "agents",
            "axPixelContradiction",
            "consequential",
            "successFlag",
        ] {
            assert!(
                world_required.contains(&field),
                "world required missing {field}: {world_required:?}"
            );
        }
        let surface_required: Vec<&str> = schema["$defs"]["surface"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for field in [
            "surfaceId",
            "conflictDomain",
            "isolated",
            "appId",
            "windowId",
            "generation",
            "displayName",
            "geometry",
            "sensitivity",
            "elements",
            "frameRegions",
        ] {
            assert!(
                surface_required.contains(&field),
                "surface required missing {field}: {surface_required:?}"
            );
        }
        let world_json = to_canonical_json(&crate::catalog::catalog()[0].world).unwrap();
        let world_value: serde_json::Value = serde_json::from_str(&world_json).unwrap();
        for field in &world_required {
            assert!(
                world_value.get(field).is_some(),
                "catalog world omitted required {field}"
            );
        }
        let surface_value = &world_value["surfaces"][0];
        for field in &surface_required {
            assert!(
                surface_value.get(field).is_some(),
                "catalog surface omitted required {field}"
            );
        }
    }

    #[test]
    fn extra_event_keys_fail_closed() {
        let ok = r#"{"atStep":0,"phase":"step_start","event":{"type":"takeover"}}"#;
        crate::schema::parse_strict::<crate::host::ScheduledEvent>(ok).unwrap();
        let extra = r#"{"atStep":0,"phase":"step_start","event":{"type":"takeover","secret":"x"}}"#;
        assert!(
            parse_strict::<crate::host::ScheduledEvent>(extra).is_err(),
            "extra event keys must fail closed"
        );
    }

    #[test]
    fn closed_adapter_and_trace_enums_fail_closed_at_runtime_and_in_schema() {
        let campaign = crate::report::run_campaign(1, crate::types::DEFAULT_SEED).unwrap();
        let mut report = serde_json::to_value(&campaign.report).unwrap();
        report["episodes"][0]["adapter"] = serde_json::json!("future_unreviewed_adapter");
        assert!(parse_strict::<crate::report::CampaignReport>(&report.to_string()).is_err());

        let mut evidence = serde_json::to_value(&campaign.evidence).unwrap();
        evidence["items"][0]["trace"][0]["kind"] = serde_json::json!("untyped_event");
        assert!(parse_strict::<crate::report::EvidenceSet>(&evidence.to_string()).is_err());

        for schema in [RESULT_SCHEMA_JSON, EVIDENCE_SCHEMA_JSON] {
            assert!(schema.contains("frontier_multimodal"));
            assert!(!schema.contains("\"adapter\": { \"type\": \"string\" }"));
        }
    }

    #[test]
    fn element_schema_required_null_fields_match_element_spec() {
        let schema: serde_json::Value = serde_json::from_str(SCENARIO_SCHEMA_JSON).unwrap();
        let required: Vec<&str> = schema["$defs"]["element"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for field in ["context", "value", "effect"] {
            assert!(
                required.contains(&field),
                "element required missing {field}: {required:?}"
            );
        }
        let json =
            to_canonical_json(&crate::catalog::catalog()[0].world.surfaces[0].elements[0]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        for field in ["context", "value", "effect"] {
            assert!(
                value.get(field).is_some(),
                "catalog element omitted {field}"
            );
        }
        let parsed: crate::host::ElementSpec = parse_strict(&json).unwrap();
        assert_eq!(
            parsed.stable_key,
            crate::catalog::catalog()[0].world.surfaces[0].elements[0].stable_key
        );
        let mut extra = value;
        extra
            .as_object_mut()
            .unwrap()
            .insert("secret".into(), serde_json::json!("needle"));
        assert!(parse_strict::<crate::host::ElementSpec>(&extra.to_string()).is_err());
    }
}
