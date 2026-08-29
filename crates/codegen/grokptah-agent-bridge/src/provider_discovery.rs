//! Bounded model discovery for OpenAI-compatible provider profiles (#278).

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use crate::gateway_config::{ProviderKind, ProviderModel};

const MAX_CATALOG_BYTES: usize = 1024 * 1024;

pub async fn discover_profile_models(profile_id: &str) -> Result<Vec<ProviderModel>> {
    let config = crate::gateway_config::load();
    let profile = config
        .profile(profile_id)
        .cloned()
        .ok_or_else(|| anyhow!("unknown provider profile `{profile_id}`"))?;
    if profile.kind != ProviderKind::OpenAiCompatible {
        bail!("provider `{profile_id}` does not use compatible model discovery");
    }
    crate::gateway_config::validate_base_url(&profile.base_url).map_err(anyhow::Error::msg)?;
    let credentials = crate::auth_store::resolve_provider_credentials(profile_id, None)
        .map_err(anyhow::Error::msg)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build provider discovery client")?;

    let mut best = Vec::new();
    let mut failures = Vec::new();
    for url in catalog_urls(&profile.base_url) {
        let mut request = client
            .get(&url)
            .header("Accept", "application/json")
            .header("User-Agent", format!("GrokPtah/{}", crate::BRIDGE_VERSION));
        if let Some(credentials) = credentials.as_ref() {
            request =
                crate::auth_store::apply_auth_headers(request, credentials, &profile.base_url);
        }
        let credential_secret = credentials
            .as_ref()
            .map(|credentials| credentials.bearer.as_bytes())
            .unwrap_or_default();
        let mut response = match crate::provider_transport::send_provider_request(
            &client,
            request,
            crate::provider_transport::ProviderRequestScope {
                credential_secret,
                dialect: "openai_model_catalog",
                model: "model-catalog",
                target_scope: "provider-model-discovery",
            },
            None,
        )
        .await
        {
            Ok(response) => response,
            Err(error) if error.is_safe_to_resend() => {
                failures.push(format!(
                    "{}: {}",
                    redacted_path(&url),
                    "provider catalog could not connect"
                ));
                continue;
            }
            Err(error) => return Err(anyhow!(error.to_string())),
        };
        let status = response.status();
        if status.is_redirection() {
            let _ = read_catalog_body(&mut response).await?;
            response.settle_success().map_err(anyhow::Error::new)?;
            failures.push(format!(
                "{}: redirect refused ({status})",
                redacted_path(&url)
            ));
            continue;
        }
        let body = match read_catalog_body(&mut response).await {
            Ok(body) => body,
            Err(error) => {
                failures.push(format!("{}: {error}", redacted_path(&url)));
                continue;
            }
        };
        if !status.is_success() {
            let class = match status.as_u16() {
                401 | 403 => "credential rejected",
                429 => "rate limited",
                _ => "request failed",
            };
            failures.push(format!("{}: {class} ({status})", redacted_path(&url)));
            continue;
        }
        match parse_compatible_model_catalog(body.as_bytes()) {
            Ok(models) => {
                response.settle_success().map_err(anyhow::Error::new)?;
                if models.len() > best.len() {
                    best = models;
                }
            }
            Err(error) => {
                let error = response.settle_protocol_error(error.to_string());
                return Err(anyhow!(error));
            }
        }
    }

    if best.is_empty() {
        bail!(
            "provider model discovery returned no usable models{}",
            if failures.is_empty() {
                String::new()
            } else {
                format!(": {}", failures.join("; "))
            }
        );
    }

    if !profile.managed_by_env {
        let mut updated = crate::gateway_config::load_for_update()
            .context("read provider profiles for model discovery")?;
        let target = updated
            .profile_mut(profile_id)
            .ok_or_else(|| anyhow!("provider `{profile_id}` disappeared during discovery"))?;
        for discovered in &mut best {
            if let Some(existing) = target.models.iter().find(|model| model.id == discovered.id) {
                discovered.capabilities = existing.capabilities.clone();
                if existing.display_name != existing.id {
                    discovered.display_name.clone_from(&existing.display_name);
                }
            }
            target.upsert_model(discovered.clone());
        }
        crate::gateway_config::save(&updated).context("save discovered provider models")?;
    }

    Ok(best)
}

async fn read_catalog_body(
    response: &mut crate::provider_transport::ProviderResponse,
) -> Result<String> {
    let mut body = crate::sse::BoundedBodyAccumulator::with_max_bytes(MAX_CATALOG_BYTES);
    while let Some(chunk) = response.next_chunk(None).await? {
        body.push(&chunk)
            .context("catalog exceeds bounded response size")?;
    }
    body.finish().context("catalog is not valid UTF-8")
}

fn catalog_urls(base_url: &str) -> Vec<String> {
    let base = base_url.trim().trim_end_matches('/');
    let mut urls = Vec::new();
    if base.ends_with("/v1") {
        urls.push(format!("{base}/models"));
    } else {
        urls.push(format!("{base}/v1/models"));
        urls.push(format!("{base}/models"));
    }
    urls
}

fn redacted_path(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return "provider endpoint".into();
    };
    let path = rest.find('/').map(|index| &rest[index..]).unwrap_or("/");
    format!("provider endpoint {path}")
}

pub fn parse_compatible_model_catalog(body: &[u8]) -> Result<Vec<ProviderModel>> {
    let value: serde_json::Value = serde_json::from_slice(body).context("invalid catalog JSON")?;
    let arrays: Vec<&Vec<serde_json::Value>> = if let Some(array) = value.as_array() {
        vec![array]
    } else {
        ["data", "models"]
            .iter()
            .filter_map(|key| value.get(key).and_then(serde_json::Value::as_array))
            .collect()
    };
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for array in arrays {
        for item in array {
            let id = item.as_str().or_else(|| {
                item.get("id")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| item.get("name").and_then(serde_json::Value::as_str))
                    .or_else(|| item.get("model").and_then(serde_json::Value::as_str))
            });
            let Some(id) = id.filter(|id| !id.trim().is_empty()) else {
                continue;
            };
            if seen.insert(id.to_string()) {
                models.push(ProviderModel::unqualified(id));
            }
        }
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_catalog_shapes_without_rewriting_ids() {
        for fixture in [
            br#"{"data":[{"id":"Team/Code:Cheap"},{"id":"Team/Code:Cheap"}]}"#.as_slice(),
            br#"{"models":[{"name":"Team/Code:Cheap"}]}"#.as_slice(),
            br#"[{"model":"Team/Code:Cheap"}]"#.as_slice(),
            br#"["Team/Code:Cheap"]"#.as_slice(),
        ] {
            let models = parse_compatible_model_catalog(fixture).unwrap();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].id, "Team/Code:Cheap");
        }

        let models = parse_compatible_model_catalog(
            r#"{"data":[{"id":" Team/Code:Cheap/日本語 "},{"id":"   "}]}"#.as_bytes(),
        )
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, " Team/Code:Cheap/日本語 ");
    }

    #[test]
    fn malformed_or_unrelated_catalogs_fail_or_stay_empty() {
        assert!(parse_compatible_model_catalog(b"not json").is_err());
        assert!(parse_compatible_model_catalog(br#"{"ok":true}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn catalog_urls_do_not_duplicate_v1() {
        assert_eq!(
            catalog_urls("https://gateway.example/v1"),
            vec!["https://gateway.example/v1/models"]
        );
        assert_eq!(
            catalog_urls("https://gateway.example/api"),
            vec![
                "https://gateway.example/api/v1/models",
                "https://gateway.example/api/models"
            ]
        );
    }
}
