//! Canonical JSON bytes for MAC inputs (#443).
//!
//! MAC inputs must be byte-stable across processes, serde versions, and the
//! `serde_json/preserve_order` feature. Relying on `serde_json`'s map ordering
//! would make every chain tag depend on feature unification elsewhere in the
//! dependency graph, so the canonical form is produced explicitly here.
//!
//! Canonical form:
//! - object keys sorted by UTF-8 byte order, duplicates impossible (`BTreeMap`);
//! - no insignificant whitespace;
//! - integers only — a float or a non-finite number is rejected rather than
//!   silently rounded into a different MAC input;
//! - string escaping delegated to `serde_json` so it matches the wire form.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use super::{AuditError, AuditResult};

/// Canonical bytes for a value with the `mac` field removed.
///
/// Every authenticated document authenticates itself minus its own tag; doing
/// the removal in one place keeps sign and verify from drifting apart.
pub(crate) fn canonical_bytes_without_mac<T: Serialize>(value: &T) -> AuditResult<Vec<u8>> {
    let mut value = serde_json::to_value(value)
        .map_err(|error| AuditError::Io(format!("canonicalize: {error}")))?;
    match value.as_object_mut() {
        Some(map) => {
            map.remove("mac");
        }
        None => {
            return Err(AuditError::Io(
                "authenticated document is not an object".into(),
            ))
        }
    }
    canonical_value_bytes(&value)
}

pub(crate) fn canonical_value_bytes(value: &Value) -> AuditResult<Vec<u8>> {
    let mut out = Vec::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut Vec<u8>) -> AuditResult<()> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => {
            if number.as_f64().is_some() && number.as_i64().is_none() && number.as_u64().is_none() {
                return Err(AuditError::Io(
                    "canonical json rejects non-integer numbers".into(),
                ));
            }
            out.extend_from_slice(number.to_string().as_bytes());
        }
        Value::String(text) => {
            let encoded = serde_json::to_string(text)
                .map_err(|error| AuditError::Io(format!("canonicalize string: {error}")))?;
            out.extend_from_slice(encoded.as_bytes());
        }
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_value(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            out.push(b'{');
            for (index, (key, item)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                let encoded = serde_json::to_string(key)
                    .map_err(|error| AuditError::Io(format!("canonicalize key: {error}")))?;
                out.extend_from_slice(encoded.as_bytes());
                out.push(b':');
                write_value(item, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}
