//! The immutable carrier that authorises exactly one physical provider request.
//!
//! [`crate::launch`] answers *may* this host start work. [`crate::attempt`]
//! answers *did* a request leave. Between them sits the question both of those
//! quietly assumed away: **is the request about to go out on the wire the same
//! request that was admitted?**
//!
//! On the previous revision it was not. Admission captured the session's model
//! and effort, the gate then re-read current state asynchronously, and the
//! transport re-resolved credentials, profile, and route again just before
//! sending. Three readings, no guarantee any two agreed, and the durable
//! record described the first one.
//!
//! A [`ResolvedRequest`] closes that by carrying the **exact bytes** that will
//! be sent, alongside the complete binding they were resolved under. It is
//! deliberately:
//!
//! - **not `Serialize`** — it holds request bytes, which contain user content.
//!   Only its [`RequestBinding`] is persistable, and that carries a digest.
//! - **not `Clone`-into-mutation** — there is no setter, and no field is
//!   public. What was admitted is what is available to send.
//! - **self-digesting** — [`ResolvedRequest::seal`] computes the digest from
//!   the bytes it is handed. A caller cannot present bytes under some other
//!   request's digest, because it never supplies the digest at all.
//!
//! The transport takes `&ResolvedRequest` and reads its bytes. That makes
//! "send without admission" a compile error rather than a review comment.

use serde::{Deserialize, Serialize};

use crate::account::{AccountReference, CredentialMethod};
use crate::attempt::{AttemptRoute, AttemptSubject, AuthorityRevisions, BoundedId};
use crate::launch::{BaseCategory, ModelReference, ProviderClass, RequestDialect, RouteClass};

/// Stable contract identifier for the resolved-request binding.
pub const GROK_RESOLVED_CONTRACT_VERSION: &str = "grokptah.resolved.v1";
/// Numeric schema revision carried in every binding.
pub const GROK_RESOLVED_SCHEMA_VERSION: u32 = 1;

/// A digest of the complete canonical request.
///
/// "Complete" is the point. The previous revision digested the prompt alone,
/// which left the system preamble, the conversation history, the tool
/// declarations, the model, and the effort outside the binding — every one of
/// which changes what the provider is asked to do and what it costs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestDigest(String);

impl RequestDigest {
    /// Digest the exact bytes that will be transmitted.
    ///
    /// Takes the serialized body rather than a structured value on purpose:
    /// two structurally equal JSON values can serialize to different bytes,
    /// and it is the bytes the provider sees.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(format!("sha256:{}", hex_sha256(bytes)))
    }

    /// The digest value, in `sha256:<hex>` form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this digest describes the given bytes.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        *self == Self::of_bytes(bytes)
    }

    /// The bounded identifier form, for embedding in an attempt record.
    pub fn as_bounded(&self) -> BoundedId {
        BoundedId::new(&self.0).expect("a sha256 digest is always a bounded identifier")
    }
}

/// Minimal SHA-256, so the contract crate keeps its zero-dependency posture.
///
/// The SDK deliberately depends only on `serde`; pulling a hashing crate in
/// for one digest would widen the dependency surface of every consumer of
/// these contracts.
fn hex_sha256(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in w.iter_mut().enumerate().take(16) {
            let base = index * 4;
            *word = u32::from_be_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

/// The exact endpoint one request is bound to.
///
/// The host is bound to a *specific* endpoint, not merely to a category: two
/// compatible providers both classified `compatible_https` are different
/// destinations, and a run admitted against one must not silently send to the
/// other. So the category is kept for humans and a digest of the exact base
/// URL is kept for comparison — the URL itself is never published, because it
/// can carry a private hostname, a tenant name, or a path secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointIdentity {
    /// Coarse classification, safe to show an operator.
    pub category: BaseCategory,
    /// Opaque digest of the exact base URL this request is bound to.
    pub fingerprint: BoundedId,
}

impl EndpointIdentity {
    /// Bind to an exact base URL without ever publishing it.
    pub fn of_base_url(category: BaseCategory, base_url: &str) -> Self {
        let digest = hex_sha256(base_url.trim_end_matches('/').as_bytes());
        Self {
            category,
            fingerprint: BoundedId::new(&format!("ep:{}", &digest[..32]))
                .expect("a truncated hex digest with a fixed prefix is bounded"),
        }
    }

    /// Whether this identity describes the given base URL.
    pub fn matches(&self, base_url: &str) -> bool {
        Self::of_base_url(self.category, base_url).fingerprint == self.fingerprint
    }
}

/// The persistable half of a resolved request.
///
/// Everything here is safe to write to disk and to hand to an operator: no
/// bytes, no URL, no credential material, no user content — only the digest of
/// what was sent and the identities it was sent under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestBinding {
    /// Stable contract identifier.
    pub contract: String,
    /// Numeric schema revision.
    pub schema_version: u32,
    /// Who and where this request acts for.
    pub subject: AttemptSubject,
    /// Every authority revision it was resolved under.
    pub authority: AuthorityRevisions,
    /// Provider family.
    pub provider: ProviderClass,
    /// The exact provider profile selected, not inferred from the family.
    pub profile: BoundedId,
    /// The exact endpoint, as a category plus an opaque fingerprint.
    pub endpoint: EndpointIdentity,
    /// Request route.
    pub route: RouteClass,
    /// Request dialect.
    pub dialect: RequestDialect,
    /// The exact wire model this request names.
    pub model: ModelReference,
    /// The exact reasoning effort this request carries.
    pub effort: BoundedId,
    /// How the credential was obtained.
    pub credential_method: CredentialMethod,
    /// Which revision of that credential is in use.
    ///
    /// A refresh increments this, so a request sent under a rotated credential
    /// is distinguishable from one sent under the credential it was admitted
    /// with — the case a turn-level precheck cannot see at all.
    pub credential_revision: u64,
    /// Bounded account handle this request bills against, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_reference: Option<AccountReference>,
    /// Digest of the complete canonical request body.
    pub digest: RequestDigest,
    /// Byte length of that body, so a truncated send is detectable.
    pub body_len: u64,
    /// Revision of the host source that produced this binding.
    pub source_revision: BoundedId,
}

impl RequestBinding {
    /// Project this binding onto the route shape an attempt records.
    pub fn attempt_route(&self) -> AttemptRoute {
        AttemptRoute {
            provider: self.provider,
            profile: Some(self.profile.clone()),
            credential_method: self.credential_method,
            route: self.route,
            base: self.endpoint.category,
            dialect: self.dialect,
            model: self.model.clone(),
            effort: Some(self.effort.clone()),
            account_reference: self.account_reference.clone(),
        }
    }

    /// Validate a binding before writing or publishing it.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract != GROK_RESOLVED_CONTRACT_VERSION {
            return Err("resolved-request contract identifier does not match this revision");
        }
        if self.schema_version != GROK_RESOLVED_SCHEMA_VERSION {
            return Err("resolved-request schema version does not match this revision");
        }
        self.subject.validate()?;
        if !self.profile.is_bounded()
            || !self.effort.is_bounded()
            || !self.endpoint.fingerprint.is_bounded()
            || !self.source_revision.is_bounded()
        {
            return Err("resolved-request binding carries an unbounded identifier");
        }
        if ModelReference::new(&self.model.value).as_ref() != Some(&self.model) {
            return Err("resolved-request model is not a bounded opaque identifier");
        }
        if self.body_len == 0 {
            return Err("a resolved request cannot have an empty body");
        }
        if BoundedId::new(self.digest.as_str()).is_none() {
            return Err("resolved-request digest is not a bounded identifier");
        }
        Ok(())
    }
}

/// One admitted request, plus the exact bytes it authorises.
///
/// Obtainable only from [`ResolvedRequest::seal`], which computes the digest
/// itself. There is no way to pair a binding with bytes it does not describe.
#[derive(Debug)]
pub struct ResolvedRequest {
    binding: RequestBinding,
    body: Vec<u8>,
}

impl ResolvedRequest {
    /// Seal a binding around the exact bytes that will be transmitted.
    ///
    /// The caller supplies everything *except* the digest and length, which
    /// are derived here. That asymmetry is the point: a forged pairing of
    /// bytes and digest is not expressible.
    #[allow(clippy::too_many_arguments)] // Every binding is deliberate and explicit.
    pub fn seal(parts: ResolvedRequestParts, body: Vec<u8>) -> Result<Self, &'static str> {
        if body.is_empty() {
            return Err("a resolved request cannot have an empty body");
        }
        let binding = RequestBinding {
            contract: GROK_RESOLVED_CONTRACT_VERSION.to_string(),
            schema_version: GROK_RESOLVED_SCHEMA_VERSION,
            subject: parts.subject,
            authority: parts.authority,
            provider: parts.provider,
            profile: parts.profile,
            endpoint: parts.endpoint,
            route: parts.route,
            dialect: parts.dialect,
            model: parts.model,
            effort: parts.effort,
            credential_method: parts.credential_method,
            credential_revision: parts.credential_revision,
            account_reference: parts.account_reference,
            digest: RequestDigest::of_bytes(&body),
            body_len: body.len() as u64,
            source_revision: parts.source_revision,
        };
        binding.validate()?;
        Ok(Self { binding, body })
    }

    /// The persistable binding.
    pub fn binding(&self) -> &RequestBinding {
        &self.binding
    }

    /// The exact bytes to transmit.
    ///
    /// This is the only way to obtain a request body for sending, which is
    /// what makes an unadmitted send a compile error rather than a convention.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Re-verify that the carried bytes still match the sealed digest.
    ///
    /// Nothing in this type can mutate the body, so this is not defending
    /// against a setter that does not exist. It is the assertion a caller
    /// makes immediately before `.send()`, so that the record saying "these
    /// bytes were sent" is checked against the bytes actually handed to the
    /// transport rather than assumed.
    pub fn verify_intact(&self) -> Result<(), &'static str> {
        if !self.binding.digest.matches(&self.body) {
            return Err("the request body does not match the digest it was admitted under");
        }
        if self.binding.body_len != self.body.len() as u64 {
            return Err("the request body length does not match its binding");
        }
        Ok(())
    }
}

/// The parts a caller supplies to [`ResolvedRequest::seal`].
///
/// A struct rather than a long argument list so a new binding field cannot be
/// silently defaulted at one call site and set at another.
#[derive(Debug, Clone)]
pub struct ResolvedRequestParts {
    /// Who and where this request acts for.
    pub subject: AttemptSubject,
    /// Every authority revision it was resolved under.
    pub authority: AuthorityRevisions,
    /// Provider family.
    pub provider: ProviderClass,
    /// The exact provider profile selected.
    pub profile: BoundedId,
    /// The exact endpoint.
    pub endpoint: EndpointIdentity,
    /// Request route.
    pub route: RouteClass,
    /// Request dialect.
    pub dialect: RequestDialect,
    /// The exact wire model.
    pub model: ModelReference,
    /// The exact reasoning effort.
    pub effort: BoundedId,
    /// How the credential was obtained.
    pub credential_method: CredentialMethod,
    /// Which revision of that credential is in use.
    pub credential_revision: u64,
    /// Bounded account handle, when published.
    pub account_reference: Option<AccountReference>,
    /// Revision of the host source that produced this binding.
    pub source_revision: BoundedId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountReferenceSource;

    fn bounded(value: &str) -> BoundedId {
        BoundedId::new(value).unwrap_or_else(|| panic!("{value:?} should be bounded"))
    }

    fn parts() -> ResolvedRequestParts {
        ResolvedRequestParts {
            subject: AttemptSubject {
                principal: Some(bounded("prn-0a1b2c3d")),
                tenant: Some(bounded("tnt-9z8y")),
                project: Some(bounded("prj-alpha")),
                workspace: bounded("wsp:0a1b2c3d"),
                session: bounded("ses:4e5f6a7b"),
            },
            authority: AuthorityRevisions {
                auth: crate::attempt::Revision(7),
                policy: crate::attempt::Revision(3),
                capability: crate::attempt::Revision(11),
                credential: crate::attempt::Revision(2),
            },
            provider: ProviderClass::Xai,
            profile: bounded("xai"),
            endpoint: EndpointIdentity::of_base_url(
                BaseCategory::XaiOfficial,
                "https://api.x.ai/v1",
            ),
            route: RouteClass::XaiFirstParty,
            dialect: RequestDialect::XaiChatCompletions,
            model: ModelReference::new("grok-4").expect("bounded model"),
            effort: bounded("high"),
            credential_method: CredentialMethod::GrokBuildOidc,
            credential_revision: 2,
            account_reference: AccountReference::new(
                "usr-0a1b2c3d",
                AccountReferenceSource::UserId,
            ),
            source_revision: bounded("src:4a4748f2"),
        }
    }

    /// The digest is the whole basis of "what was admitted is what was sent",
    /// so it is checked against independently computed vectors rather than
    /// against itself. The `0`-repeat cases bracket every padding boundary a
    /// hand-rolled implementation gets wrong: 55/56 is where the 64-bit length
    /// field stops fitting in the final block, and 63/64/65 straddle a whole
    /// block.
    #[test]
    fn the_digest_matches_independently_computed_sha256_vectors() {
        for (input, expected) in [
            (
                String::new(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "abc".to_string(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".to_string(),
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                "0".repeat(55),
                "9f8ef876f51f5313c91cc3f6b8119af09d8bbdd72098fa149b2780eb3591d6be",
            ),
            (
                "0".repeat(56),
                "bd03ac1428f0ea86f4b83a731ffc7967bb82866d8545322f888d2f6e857ffc18",
            ),
            (
                "0".repeat(63),
                "c7dc2d25e306355c97af916e8d50b27a948506a74c6b2dd1b29e2b63d0a3aa8c",
            ),
            (
                "0".repeat(64),
                "60e05bd1b195af2f94112fa7197a5c88289058840ce7c6df9693756bc6250f55",
            ),
            (
                "0".repeat(65),
                "e531ef0f962409170917abf9de3287afec23dd1c42c9e1fea66c5feab99e8f7c",
            ),
            (
                "0".repeat(120),
                "09719c55365a950c92a06122b6ce2634e3ce9b6dbcde1827171941658c7eedab",
            ),
            (
                "a".repeat(1000),
                "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3",
            ),
        ] {
            assert_eq!(
                RequestDigest::of_bytes(input.as_bytes()).as_str(),
                format!("sha256:{expected}"),
                "sha256 of a {}-byte input is wrong",
                input.len()
            );
        }
        // Non-UTF-8 bytes hash too: a request body is bytes, not text.
        assert_eq!(
            RequestDigest::of_bytes(&[0x00, 0xff, 0x80, 0x7f])
                .as_str()
                .len(),
            "sha256:".len() + 64
        );
    }

    #[test]
    fn a_sealed_request_digests_the_exact_bytes_it_carries() {
        let body = br#"{"model":"grok-4","messages":[]}"#.to_vec();
        let request = ResolvedRequest::seal(parts(), body.clone()).expect("seals");
        assert_eq!(request.body(), body.as_slice());
        assert_eq!(request.binding().body_len, body.len() as u64);
        assert!(request.binding().digest.matches(&body));
        assert_eq!(request.verify_intact(), Ok(()));
        assert_eq!(request.binding().validate(), Ok(()));
    }

    /// The reason the digest is derived rather than supplied: two requests
    /// that differ anywhere in the body must not share a binding.
    #[test]
    fn any_change_to_the_body_changes_the_digest() {
        let base = br#"{"model":"grok-4","effort":"high","messages":[{"role":"user","content":"hi"}],"tools":[]}"#.to_vec();
        let sealed = ResolvedRequest::seal(parts(), base.clone()).expect("seals");
        for altered in [
            br#"{"model":"grok-3","effort":"high","messages":[{"role":"user","content":"hi"}],"tools":[]}"#.to_vec(),
            br#"{"model":"grok-4","effort":"low","messages":[{"role":"user","content":"hi"}],"tools":[]}"#.to_vec(),
            br#"{"model":"grok-4","effort":"high","messages":[{"role":"system","content":"hi"}],"tools":[]}"#.to_vec(),
            br#"{"model":"grok-4","effort":"high","messages":[{"role":"user","content":"hi!"}],"tools":[]}"#.to_vec(),
            br#"{"model":"grok-4","effort":"high","messages":[{"role":"user","content":"hi"}],"tools":[{"name":"x"}]}"#.to_vec(),
        ] {
            assert_ne!(
                RequestDigest::of_bytes(&altered),
                sealed.binding().digest,
                "a changed request kept the admitted digest"
            );
            assert!(!sealed.binding().digest.matches(&altered));
        }
    }

    /// A prompt-only digest could not tell these apart; this one can.
    #[test]
    fn the_digest_covers_system_history_tools_model_and_effort() {
        let prompt = "summarise the repository";
        let with_history = format!(
            r#"{{"model":"grok-4","messages":[{{"role":"user","content":"earlier"}},{{"role":"user","content":"{prompt}"}}]}}"#
        );
        let without_history =
            format!(r#"{{"model":"grok-4","messages":[{{"role":"user","content":"{prompt}"}}]}}"#);
        assert_ne!(
            RequestDigest::of_bytes(with_history.as_bytes()),
            RequestDigest::of_bytes(without_history.as_bytes()),
            "history is outside the digest"
        );
    }

    #[test]
    fn an_endpoint_identity_pins_the_exact_base_url_without_publishing_it() {
        let official =
            EndpointIdentity::of_base_url(BaseCategory::XaiOfficial, "https://api.x.ai/v1");
        assert!(official.matches("https://api.x.ai/v1"));
        // A trailing slash is the same endpoint.
        assert!(official.matches("https://api.x.ai/v1/"));
        // A different host is not, even in the same category.
        assert!(!official.matches("https://api.x.ai/v2"));
        assert!(!official.matches("https://evil.example/v1"));

        // Two compatible providers in one category stay distinguishable.
        let first =
            EndpointIdentity::of_base_url(BaseCategory::CompatibleHttps, "https://a.example/v1");
        let second =
            EndpointIdentity::of_base_url(BaseCategory::CompatibleHttps, "https://b.example/v1");
        assert_eq!(first.category, second.category);
        assert_ne!(
            first.fingerprint, second.fingerprint,
            "two different endpoints in one category collapsed to the same identity"
        );

        // And the URL never appears in the published form.
        let encoded = serde_json::to_string(&official).expect("serializes");
        assert!(!encoded.contains("api.x.ai"));
        assert!(!encoded.contains("https://"));
    }

    #[test]
    fn a_binding_publishes_no_bytes_credentials_or_endpoint() {
        let body = br#"{"messages":[{"role":"user","content":"my secret plan"}]}"#.to_vec();
        let request = ResolvedRequest::seal(parts(), body).expect("seals");
        let encoded = serde_json::to_string(request.binding()).expect("binding serializes");
        for forbidden in [
            "my secret plan",
            "messages",
            "Bearer",
            "bearer",
            "refresh_token",
            "apiKey",
            "https://",
            "api.x.ai",
            "/Users/",
            "/home/",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "the binding leaked {forbidden:?}: {encoded}"
            );
        }
        // What it does carry is the digest and the length.
        assert!(encoded.contains("sha256:"));
        assert!(encoded.contains("bodyLen"));
    }

    #[test]
    fn an_empty_body_is_refused() {
        assert!(ResolvedRequest::seal(parts(), Vec::new()).is_err());
    }

    #[test]
    fn the_credential_revision_distinguishes_a_refreshed_credential() {
        let body = br#"{"model":"grok-4"}"#.to_vec();
        let before = ResolvedRequest::seal(parts(), body.clone()).expect("seals");
        let mut rotated = parts();
        rotated.credential_revision = 3;
        let after = ResolvedRequest::seal(rotated, body).expect("seals");
        assert_eq!(before.binding().digest, after.binding().digest);
        assert_ne!(
            before.binding().credential_revision,
            after.binding().credential_revision,
            "a rotated credential was indistinguishable from the admitted one"
        );
        assert_ne!(before.binding(), after.binding());
    }

    #[test]
    fn a_binding_round_trips_and_pins_its_wire_shape() {
        let request =
            ResolvedRequest::seal(parts(), br#"{"model":"grok-4"}"#.to_vec()).expect("seals");
        let encoded = serde_json::to_value(request.binding()).expect("serializes");
        let decoded: RequestBinding = serde_json::from_value(encoded.clone()).expect("round-trips");
        assert_eq!(&decoded, request.binding());
        let mut keys: Vec<&str> = encoded
            .as_object()
            .expect("binding is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "accountReference",
                "authority",
                "bodyLen",
                "contract",
                "credentialMethod",
                "credentialRevision",
                "dialect",
                "digest",
                "effort",
                "endpoint",
                "model",
                "profile",
                "provider",
                "route",
                "schemaVersion",
                "sourceRevision",
                "subject",
            ]
        );
    }

    #[test]
    fn a_doctored_binding_is_refused_by_its_own_validator() {
        let request =
            ResolvedRequest::seal(parts(), br#"{"model":"grok-4"}"#.to_vec()).expect("seals");
        let base = serde_json::to_value(request.binding()).expect("serializes");

        for (label, mutate) in [
            (
                "contract",
                Box::new(|value: &mut serde_json::Value| {
                    value["contract"] = serde_json::json!("grokptah.resolved.v2");
                }) as Box<dyn Fn(&mut serde_json::Value)>,
            ),
            (
                "empty body",
                Box::new(|value: &mut serde_json::Value| {
                    value["bodyLen"] = serde_json::json!(0);
                }),
            ),
            (
                "unbounded profile",
                Box::new(|value: &mut serde_json::Value| {
                    value["profile"] = serde_json::json!("../../etc/passwd");
                }),
            ),
            (
                "unbounded model",
                Box::new(|value: &mut serde_json::Value| {
                    value["model"]["value"] = serde_json::json!("grok-4 <script>");
                }),
            ),
        ] {
            let mut doctored = base.clone();
            mutate(&mut doctored);
            let decoded: RequestBinding =
                serde_json::from_value(doctored).expect("still decodes structurally");
            assert!(decoded.validate().is_err(), "{label} validated");
        }

        let mut extra = base;
        extra["balanceUsd"] = serde_json::json!(42);
        assert!(
            serde_json::from_value::<RequestBinding>(extra).is_err(),
            "deny_unknown_fields let an extra claim through"
        );
    }
}
