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
}
