# Grok Build live credential attestation

This module is a production safety seam for a future live Persistent-Agent
certification campaign. It does not enable live certification by itself.

`attest_grok_build_oidc` returns a positive-schema
`LiveCredentialAttestation`. The serialized value contains only stable enums,
booleans, public `grok-*` model identifiers, and opaque hashes. It never returns
tokens, client identifiers, subjects, user or team identifiers, filesystem
paths, arbitrary URLs, or provider response bodies.

The attestation fails closed when an xAI API key, API-base override, token
command, compatible-gateway environment, auth configuration override, keychain
API key, ambiguous credential, unsafe auth file, non-canonical issuer, or
non-canonical model route is observed. The auth file must be a bounded regular
file owned by the current user, mode `0600`, with one link and no symbolic-link
ancestor. Inspection binds the opened inode and rejects changes during the
read. `GROK_HOME`, when set, is the official cache-location override and is
used consistently by credential resolution, refresh, and attestation.

The canonical issuer is `https://auth.x.ai`, and the Grok Build request endpoint
is `https://cli-chat-proxy.grok.com/v1`. The refresh endpoint policy is pinned to
`https://auth.x.ai/oauth2/token`; that path was derived from the installed
official Grok 1.0.5 CLI contract. OIDC discovery must agree exactly and may not
widen the policy. Refresh requests deny redirects and bound connection time,
operation time, response bytes, JSON shape, and token fields. Errors are stable
codes and never retain response prose or URLs.

Refreshed credentials are installed with a unique, exclusive, same-directory
`0600` temporary file. The writer flushes and syncs the file, verifies its
metadata, rechecks the original inode and bytes, atomically renames, syncs the
parent directory, and removes only its exact temporary file on failure.

## Bounded campaign policy

The user's OIDC client identifier is dynamic and is not an authoritative
constant in this repository. The implementation does not guess or hard-code
one. Direct Grok Build proxy requests use the cached bearer and the
first-party token-auth header, so the dynamic client identifier is not treated
as route authority. A live campaign must first be established or refreshed by
the official Grok CLI, then the strict resolver requires the cached token to
outlive the complete campaign bound. This keeps the first certification path
from depending on an invented in-process OAuth refresh contract.

`attest_grok_build_oidc_with_min_validity` is the campaign entry point. It
returns `oidc_session_expires_too_soon` before host startup if the bounded
window cannot be met. Provider observations carry an opaque credential binding
only after this positive-schema route and credential attestation succeeds.

Provider observations have an optional opaque credential binding. The runtime
adds it only when `LiveCredentialAttestation::certification_ready()` is true.
Missing or mismatched binding, incomplete authoritative usage, or observation
recorder drops must remain indeterminate in the certification lab.

The provider-quota receipt set is fail-closed: the consumption receipt must
show at least one request and token, while the exhaustion receipt must be an
observed HTTP 429 with zero successful requests and zero successful tokens.
This prevents a claimed exhaustion event from smuggling a second consumption
quantity into the secret-free evidence.

The candidate also exposes `LiveProviderCampaignEvidence`, a secret-free
positive projection that can be assembled only from a ready attestation and a
validated provider-quota receipt set. It binds the named campaign, opaque
credential fingerprint, canonical route/model digest, consumption receipt, and
HTTP-429 exhaustion receipt, then records an evidence digest for transport
tamper detection. Constructing this object does not itself constitute a live
campaign; the operator must still run the named catalog and attach the
resulting artifact.
