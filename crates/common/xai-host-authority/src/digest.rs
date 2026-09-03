//! Content digests used to bind authority to exactly one action.
//!
//! Digests are the only way content enters an authority record. The bytes
//! themselves are never stored, so a durable authority record — and every
//! projection derived from it — is content-free by construction.

use sha2::{Digest, Sha256};

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// A short, domain-separated, non-reversible handle for public projections.
pub(crate) fn short_handle(label: &str, bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"xai-host-authority/public-handle/v1\0");
    h.update(label.as_bytes());
    h.update(b"\0");
    h.update(bytes);
    let digest = h.finalize();
    format!("{label}_{}", hex(&digest[..8]))
}

/// A SHA-256 digest of some action or body, bound into an authority record.
// Not `Serialize`/`Deserialize`: durable records carry hex strings, and
// keeping the derives off means there is exactly one way a digest enters this
// crate - by being computed over real bytes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    /// Digest raw bytes. Public: computing a digest grants nothing on its own.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(b"xai-host-authority/content/v1\0");
        h.update(bytes);
        Self(h.finalize().into())
    }

    /// Digest a sequence of labelled fields with unambiguous framing.
    ///
    /// Each field is length-prefixed so that no two distinct field sequences
    /// can collide by concatenation — `("ab","c")` and `("a","bc")` differ.
    pub fn of_fields(fields: &[(&str, &[u8])]) -> Self {
        let mut h = Sha256::new();
        h.update(b"xai-host-authority/fields/v1\0");
        h.update((fields.len() as u64).to_le_bytes());
        for (name, value) in fields {
            h.update((name.len() as u64).to_le_bytes());
            h.update(name.as_bytes());
            h.update((value.len() as u64).to_le_bytes());
            h.update(value);
        }
        Self(h.finalize().into())
    }

    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn to_hex(self) -> String {
        hex(&self.0)
    }

    /// Public, truncated handle. Safe to log: it identifies *which* action was
    /// authorised without revealing the action, and cannot be inverted.
    pub fn public_handle(&self) -> String {
        short_handle("dig", &self.0)
    }
}

impl std::fmt::Debug for ContentDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ContentDigest({})", self.public_handle())
    }
}

/// The full identity of one physical provider request.
///
/// A permit is bound to this whole tuple, not just the body. Changing the URL,
/// the HTTP method, the wire dialect, the credential, or the model after
/// admission invalidates the permit, so a request admitted for one endpoint or
/// one credential can never be replayed against another.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestIdentity {
    url: String,
    method: String,
    dialect: String,
    credential_fingerprint: ContentDigest,
    model: String,
    body: ContentDigest,
}

impl RequestIdentity {
    /// Build the identity of a physical send.
    ///
    /// `credential_secret` is digested immediately and never retained, so the
    /// identity binds *which* credential was used without storing it.
    pub fn new(
        url: &str,
        method: &str,
        dialect: &str,
        credential_secret: &[u8],
        model: &str,
        body: &[u8],
    ) -> Self {
        Self {
            url: url.to_string(),
            method: method.to_ascii_uppercase(),
            dialect: dialect.to_string(),
            credential_fingerprint: ContentDigest::of_fields(&[("credential", credential_secret)]),
            model: model.to_string(),
            body: ContentDigest::of_bytes(body),
        }
    }

    /// The single digest that a permit is bound to.
    pub fn digest(&self) -> ContentDigest {
        ContentDigest::of_fields(&[
            ("url", self.url.as_bytes()),
            ("method", self.method.as_bytes()),
            ("dialect", self.dialect.as_bytes()),
            ("credential", self.credential_fingerprint.as_bytes()),
            ("model", self.model.as_bytes()),
            ("body", self.body.as_bytes()),
        ])
    }

    /// Body digest alone, for audit correlation.
    pub fn body_digest(&self) -> ContentDigest {
        self.body
    }

    /// Wire dialect class bound into this identity.
    pub fn dialect(&self) -> &str {
        &self.dialect
    }
}

impl std::fmt::Debug for RequestIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the URL, model, credential, or body: a provider URL can
        // carry a key in its query string and a body carries user content.
        f.debug_struct("RequestIdentity")
            .field("digest", &self.digest())
            .finish_non_exhaustive()
    }
}
