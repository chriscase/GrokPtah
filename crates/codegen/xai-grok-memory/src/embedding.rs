//! Embedding provider abstraction for memory vector search.
//!
//! Defines the `EmbeddingProvider` trait and an API-based implementation
//! that calls an OpenAI-compatible embeddings API endpoint.
//!
//! Embeddings are cached in the sqlite-vec `chunks_vec` table — the vec0
//! virtual table IS the cache. No separate cache needed.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::AUTHORIZATION;
use xai_grok_auth::AuthCredentialProvider;
use xai_host_authority::{
    FailedReason, OperatorSendHost, PhysicalSendPermit, RequestIdentity, UncertainReason,
};

/// Proven-NotSent connect retries only. Possible-write 429/5xx responses are
/// not resent; a 401 may start a new admitted attempt after an explicit refresh.
const MAX_CONNECT_RETRIES: usize = 3;
const INITIAL_BACKOFF_MS: u64 = 1000;

/// Trait for generating text embeddings.
///
/// Implementations must be `Send + Sync` so they can be used in `Send`
/// futures (e.g., inside `tokio::spawn`). The `embed_batch` method is
/// async to support API-based providers.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a batch of texts, returning one vector per input text.
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>>;

    /// The model name used for embeddings.
    fn model_name(&self) -> &str;

    /// The dimensionality of the embedding vectors.
    fn dimensions(&self) -> usize;
}

/// API-based embedding provider using an OpenAI-compatible embeddings endpoint.
pub struct ApiEmbeddingProvider {
    api_base: String,
    model: String,
    dimensions: usize,
    credentials: Arc<dyn AuthCredentialProvider>,
    max_batch_size: usize,
}

impl ApiEmbeddingProvider {
    pub fn new(
        api_base: String,
        model: String,
        dimensions: usize,
        credentials: Arc<dyn AuthCredentialProvider>,
    ) -> Self {
        Self {
            api_base,
            model,
            dimensions,
            credentials,
            max_batch_size: 32,
        }
    }

    pub fn from_config(
        config: &xai_grok_config_types::MemoryEmbeddingConfig,
        api_base: String,
        credentials: Arc<dyn AuthCredentialProvider>,
    ) -> Option<Self> {
        let model = config.model.clone().filter(|m| !m.is_empty())?;
        Some(Self::new(api_base, model, config.dimensions, credentials))
    }

    pub fn from_session(
        config: &xai_grok_config_types::MemoryEmbeddingConfig,
        proxy_base_url: String,
        auth_key: String,
    ) -> Option<Self> {
        let credentials: Arc<dyn AuthCredentialProvider> =
            Arc::new(xai_grok_auth::StaticAuthCredentialProvider::new(
                Box::new(NoopHttpAuth),
                Some(auth_key),
            ));
        Self::from_config(config, proxy_base_url, credentials)
    }

    async fn embed_one_batch(
        &self,
        body_json: &serde_json::Value,
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        let url = format!("{}/embeddings", self.api_base);
        let mut auth_retried = false;
        let mut connect_attempt = 0usize;

        loop {
            let mut builder = xai_grok_http::shared_client()
                .post(&url)
                .json(body_json)
                .header("X-XAI-Token-Auth", "xai-grok-cli")
                .header("x-grok-client-version", xai_grok_version::VERSION)
                // A refresh is an explicit new physical attempt. Connect
                // retries keep the same ID because they are proven NotSent.
                .header(
                    "x-grok-req-id",
                    if auth_retried {
                        "embedding-auth-refresh-1"
                    } else {
                        "embedding-attempt-1"
                    },
                );
            builder = self.credentials.apply(builder, &self.api_base);
            if let Some(token) = self.credentials.snapshot().token {
                builder = builder.bearer_auth(token);
            }
            let request = builder
                .build()
                .map_err(|error| format!("failed to build embedding request: {error}"))?;
            let body = match request.body() {
                None => &[][..],
                Some(body) => body
                    .as_bytes()
                    .ok_or("embedding request body is not immutable bytes")?,
            };
            let credential = request
                .headers()
                .get(AUTHORIZATION)
                .map(|value| value.as_bytes().to_vec())
                .unwrap_or_default();
            let identity = RequestIdentity::new_with_provider_request_id(
                request.url().as_str(),
                request.method().as_str(),
                "openai_embeddings",
                &credential,
                &self.model,
                request
                    .headers()
                    .get("x-grok-req-id")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default(),
                body,
            );
            let host = OperatorSendHost::process()?;
            let (_auth, permit) = host.admit(&identity, "embeddings")?;
            let mut live = LivePermit {
                host: Arc::clone(&host),
                permit: Some(permit),
            };

            match xai_grok_http::shared_client().execute(request).await {
                Ok(response) => {
                    let status = response.status();
                    let bytes = match response.bytes().await {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            live.fail_uncertain(UncertainReason::ResponseBodyAfterPossibleEffect);
                            return Err(format!("embedding response body failed: {error}").into());
                        }
                    };
                    if status.is_success() {
                        live.complete();
                        return parse_embedding_payload(&bytes);
                    }
                    if status == reqwest::StatusCode::UNAUTHORIZED && !auth_retried {
                        live.fail_uncertain(UncertainReason::ProtocolAfterPossibleEffect);
                        if self.credentials.refresh_after_unauthorized().await {
                            auth_retried = true;
                            continue;
                        }
                        return Err(format!(
                            "embedding API error {status}: {}",
                            String::from_utf8_lossy(&bytes)
                        )
                        .into());
                    }
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
                    {
                        live.fail_uncertain(UncertainReason::ProtocolAfterPossibleEffect);
                        return Err(format!(
                            "embedding API error {status}: {}",
                            String::from_utf8_lossy(&bytes)
                        )
                        .into());
                    }
                    live.complete();
                    return Err(format!(
                        "embedding API error {status}: {}",
                        String::from_utf8_lossy(&bytes)
                    )
                    .into());
                }
                Err(error) if error.is_connect() => {
                    live.fail_before_write(FailedReason::ConnectRefusedBeforeWrite);
                    connect_attempt += 1;
                    let error_message = format!("request failed: {error}");
                    if connect_attempt >= MAX_CONNECT_RETRIES {
                        return Err(format!(
                            "embedding API failed after {MAX_CONNECT_RETRIES} connect attempts: {}",
                            error_message
                        )
                        .into());
                    }
                    let delay = INITIAL_BACKOFF_MS * 2u64.pow(connect_attempt as u32 - 1);
                    tracing::warn!(
                        attempt = connect_attempt,
                        delay_ms = delay,
                        "retrying embedding connect after proven NotSent"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                Err(error) => {
                    live.fail_uncertain(UncertainReason::TransportAfterPossibleWrite);
                    return Err(format!("request failed: {error}").into());
                }
            }
        }
    }
}

struct LivePermit {
    host: Arc<OperatorSendHost>,
    permit: Option<PhysicalSendPermit>,
}

impl LivePermit {
    fn complete(&mut self) {
        if let Some(permit) = self.permit.take() {
            let _ = self.host.settle_settled(permit);
        }
    }

    fn fail_before_write(&mut self, reason: FailedReason) {
        if let Some(permit) = self.permit.take() {
            let _ = self.host.settle_failed_before_write(permit, reason);
        }
    }

    fn fail_uncertain(&mut self, reason: UncertainReason) {
        if let Some(permit) = self.permit.take() {
            let _ = self.host.settle_uncertain(permit, reason);
        }
    }
}

impl Drop for LivePermit {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            let _ = self
                .host
                .settle_uncertain(permit, UncertainReason::TransportAfterPossibleWrite);
        }
    }
}

fn parse_embedding_payload(bytes: &[u8]) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
    let body: serde_json::Value = serde_json::from_slice(bytes)?;
    let data = body
        .get("data")
        .and_then(|value| value.as_array())
        .ok_or("embedding response missing 'data' array")?;
    let mut embeddings = Vec::with_capacity(data.len());
    for item in data {
        let embedding: Vec<f32> = item
            .get("embedding")
            .and_then(|value| value.as_array())
            .ok_or("embedding item missing 'embedding' array")?
            .iter()
            .filter_map(|value| value.as_f64().map(|float| float as f32))
            .collect();
        embeddings.push(embedding);
    }
    Ok(embeddings)
}

struct NoopHttpAuth;

impl xai_grok_auth::HttpAuth for NoopHttpAuth {
    fn apply(&self, builder: reqwest::RequestBuilder, _base_url: &str) -> reqwest::RequestBuilder {
        builder
    }
}

#[async_trait]
impl EmbeddingProvider for ApiEmbeddingProvider {
    #[tracing::instrument(name = "memory.embed_batch", skip_all, fields(batch_size = texts.len()))]
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());

        for batch in texts.chunks(self.max_batch_size) {
            let input: Vec<&str> = batch.to_vec();
            let body_json = serde_json::json!({
                "model": self.model,
                "input": input,
                "dimensions": self.dimensions,
            });
            let embeddings = self.embed_one_batch(&body_json).await?;
            all_embeddings.extend(embeddings);
        }

        Ok(all_embeddings)
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

/// A mock embedding provider for testing that returns deterministic vectors.
/// Uses blake3 hash of text → float values for reproducible results.
#[cfg(test)]
pub struct MockEmbeddingProvider {
    pub dimensions: usize,
}

#[cfg(test)]
#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        Ok(texts
            .iter()
            .map(|text| {
                let hash = blake3::hash(text.as_bytes());
                let bytes = hash.as_bytes();
                (0..self.dimensions)
                    .map(|i| bytes[i % 32] as f32 / 255.0)
                    .collect()
            })
            .collect())
    }

    fn model_name(&self) -> &str {
        "mock-embedding"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_embedding_deterministic() {
        let provider = MockEmbeddingProvider { dimensions: 4 };
        let r1 = provider.embed_batch(&["hello"]).await.unwrap();
        let r2 = provider.embed_batch(&["hello"]).await.unwrap();
        assert_eq!(r1, r2);
    }

    #[tokio::test]
    async fn test_mock_embedding_different_texts() {
        let provider = MockEmbeddingProvider { dimensions: 4 };
        let results = provider.embed_batch(&["hello", "world"]).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_ne!(results[0], results[1]);
    }

    #[tokio::test]
    async fn test_mock_embedding_empty_input() {
        let provider = MockEmbeddingProvider { dimensions: 4 };
        let results = provider.embed_batch(&[]).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_mock_embedding_correct_dimensions() {
        let provider = MockEmbeddingProvider { dimensions: 128 };
        let results = provider.embed_batch(&["test"]).await.unwrap();
        assert_eq!(results[0].len(), 128);
    }

    #[test]
    fn embedding_admits_before_single_execute_and_does_not_retry_possible_write() {
        let production = include_str!("embedding.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production embedding source");
        assert!(
            production.contains("OperatorSendHost::process") && production.contains("host.admit("),
            "embeddings must admit through OperatorSendHost before bytes"
        );
        let admit = production
            .find("host.admit(")
            .expect("embedding path must call host.admit");
        let execute = production
            .find("shared_client().execute(request)")
            .expect("embedding path must perform one shared_client execute");
        assert!(
            admit < execute,
            "OperatorSendHost admission must precede the single shared_client execute"
        );
        assert_eq!(
            production
                .matches("shared_client().execute(request)")
                .count(),
            1,
            "embedding path must have exactly one shared_client execute"
        );
        assert!(
            !production.contains(".send().await") && !production.contains("self.http.execute"),
            "embedding path must not raw-send outside the admitted execute"
        );
        let rate_limited = production
            .split(
                "if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()",
            )
            .nth(1)
            .expect("embedding path must classify 429/5xx")
            .split("live.complete()")
            .next()
            .expect("429/5xx arm");
        assert!(
            rate_limited.contains("fail_uncertain")
                && rate_limited.contains("return Err")
                && !rate_limited.contains("continue"),
            "429/5xx must settle Uncertain and return; they must not retry"
        );
        assert!(
            production.contains("UncertainReason::TransportAfterPossibleWrite")
                && production.contains("retrying embedding connect after proven NotSent"),
            "non-connect transport errors are possible-write; only proven NotSent connect retries"
        );
        assert_eq!(
            production.matches("continue;").count(),
            1,
            "the only loop continue is 401 refresh as a new admitted attempt"
        );
        let cont = production.find("continue;").expect("401 refresh continue");
        assert!(
            production[..cont].contains("UNAUTHORIZED"),
            "the only continue must belong to the 401 refresh path"
        );
    }

    fn embedding_config() -> xai_grok_config_types::MemoryEmbeddingConfig {
        xai_grok_config_types::MemoryEmbeddingConfig {
            provider: "api".into(),
            model: Some("test-embed".into()),
            dimensions: 2,
        }
    }

    async fn spawn_embeddings_server(
        handler: impl Fn(usize, axum::http::HeaderMap, axum::body::Bytes) -> axum::response::Response
        + Send
        + Sync
        + Clone
        + 'static,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use axum::Router;
        use axum::extract::State;
        use axum::routing::post;
        use tokio::net::TcpListener;

        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = hits.clone();
        let app = Router::new()
            .route(
                "/embeddings",
                post(
                    move |State(hits): State<Arc<std::sync::atomic::AtomicUsize>>,
                          headers: axum::http::HeaderMap,
                          body: axum::body::Bytes| {
                        let handler = handler.clone();
                        async move {
                            let prior = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            handler(prior, headers, body)
                        }
                    },
                ),
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), hits)
    }

    #[tokio::test]
    async fn admitted_embedding_batch_hits_fake_transport_once() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;

        let (base, hits) = spawn_embeddings_server(|_, _, _| {
            (
                StatusCode::OK,
                r#"{"data":[{"embedding":[0.1,0.2]},{"embedding":[0.3,0.4]}]}"#,
            )
                .into_response()
        })
        .await;
        let provider =
            ApiEmbeddingProvider::from_session(&embedding_config(), base, "test-key".into())
                .unwrap();
        let results = provider.embed_batch(&["a", "b"]).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].len(), 2);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn possible_write_5xx_is_not_resent() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;

        let (base, hits) =
            spawn_embeddings_server(|_, _, _| StatusCode::INTERNAL_SERVER_ERROR.into_response())
                .await;
        let provider =
            ApiEmbeddingProvider::from_session(&embedding_config(), base, "test-key".into())
                .unwrap();
        let first = provider.embed_batch(&["same"]).await;
        assert!(first.is_err(), "{first:?}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let second = provider.embed_batch(&["same"]).await;
        assert!(second.is_err(), "{second:?}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "possible-write 5xx must not admit a second send of the same body"
        );
    }

    #[tokio::test]
    async fn possible_write_429_is_not_resent() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;

        let (base, hits) =
            spawn_embeddings_server(|_, _, _| StatusCode::TOO_MANY_REQUESTS.into_response()).await;
        let provider =
            ApiEmbeddingProvider::from_session(&embedding_config(), base, "test-key".into())
                .unwrap();
        assert!(provider.embed_batch(&["rate"]).await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(provider.embed_batch(&["rate"]).await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    struct RefreshOnceProvider {
        token: std::sync::Mutex<String>,
    }

    impl xai_grok_auth::HttpAuth for RefreshOnceProvider {
        fn apply(
            &self,
            builder: reqwest::RequestBuilder,
            _base_url: &str,
        ) -> reqwest::RequestBuilder {
            builder
        }
    }

    #[async_trait]
    impl AuthCredentialProvider for RefreshOnceProvider {
        fn snapshot(&self) -> xai_grok_auth::CredentialSnapshot {
            xai_grok_auth::CredentialSnapshot {
                token: Some(self.token.lock().unwrap().clone()),
                ..Default::default()
            }
        }

        async fn refresh_after_unauthorized(&self) -> bool {
            *self.token.lock().unwrap() = "fresh-token".into();
            true
        }
    }

    #[tokio::test]
    async fn unauthorized_refresh_is_a_new_admitted_send() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;

        let (base, hits) = spawn_embeddings_server(|_, headers, _| {
            let auth = headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if auth == "Bearer stale-token" {
                StatusCode::UNAUTHORIZED.into_response()
            } else {
                (StatusCode::OK, r#"{"data":[{"embedding":[1.0,2.0]}]}"#).into_response()
            }
        })
        .await;
        let credentials: Arc<dyn AuthCredentialProvider> = Arc::new(RefreshOnceProvider {
            token: std::sync::Mutex::new("stale-token".into()),
        });
        let provider =
            ApiEmbeddingProvider::from_config(&embedding_config(), base, credentials).unwrap();
        let results = provider.embed_batch(&["hello"]).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
}
