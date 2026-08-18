# Provider profiles

GrokPtah can use its built-in xAI/Grok Build connection or named
OpenAI-compatible provider profiles. A profile binds one base URL, credential
reference, model inventory, request budget, and capability record. Routing and
authentication are resolved together in the native host.

## Configure a provider

Open **Settings > Auth > Model providers** and enter:

- a stable local profile ID and display name;
- the provider's HTTPS base URL (loopback HTTP is allowed for local gateways);
- one exact model ID;
- an API key, if required;
- an optional request budget and documented effort values.

**Save provider** makes that model the current selection. **Discover models**
tries common compatible model-catalog paths and preserves returned IDs exactly.
Manual model IDs remain available when a gateway does not expose a catalog.

Provider API keys are stored in the OS keychain. `~/.grokptah/gateway.json`
contains only profile metadata and credential references. A legacy plaintext
gateway key is copied to and verified from the keychain before the old value is
removed; a failed migration leaves the original file unchanged.

## Qualify a model

**Qualify model** sends a bounded set of synthetic prompts to the selected
provider. It never reads the active workspace. The probe checks:

- basic chat generation;
- a real native call to an inert, side-effect-free tool;
- continuation after an inert tool result;
- exact semantic selection from a deterministic local Computer Use simulator snapshot containing
  hostile observed text;
- stale-observation recovery bound to a replacement simulator frame;
- byte-safe streaming.

Tool-shaped prose does not count as tool support. A model is enabled for the
coding loop only after native tool calling and tool-result continuation pass.
Models that pass chat but fail tools remain available for discussions. Measured
capabilities are tied to the exact profile, endpoint, and model ID.

Computer capability is independent of coding capability and defaults to `none`. A successful exact
semantic selection qualifies `observe`; only successful stale-frame recovery qualifies
`semantic_act`. `visual_fallback_act` additionally requires measured image-input support and is not
granted by this first probe. No model name, ordinary tool result, effort setting, or blanket desktop
permission manufactures Computer authority. The operator cockpit remains usable manually and shows
the selected model's measured tier; local one-use approval is still required for every action.

The built-in xAI route is also resolved as a provider profile, but that profile is synthesized and
owned by the host rather than stored in `gateway.json`. The reserved `xai` ID cannot be created,
renamed, edited, or deleted by users. Its endpoint, catalog-to-wire model mapping, effort values,
deadline, and API-key or Grok Build OIDC credentials use the same profile-shaped routing path as
compatible providers. `XAI_API_BASE` remains the explicit endpoint override.

Built-in xAI models follow the same behavioral qualification proof. Measured capabilities are stored
separately from mutable provider configuration and apply only to the exact resolved endpoint, catalog
model, wire model, credential mode, non-secret credential/principal fingerprint, and qualification
schemas. API-key rotation or an OIDC principal/team change therefore invalidates the measurement;
ordinary access-token refresh for the same durable principal does not. Each exact route is retained
independently, so qualifying an API-key route cannot remove or widen an OIDC or alternate-endpoint
result. Store updates are serialized across threads and processes with an OS file lock, written
through a private unique temporary file, flushed, and atomically renamed; malformed recovery data is
never overwritten or treated as an empty store, and
xAI route resolution fails closed until it is repaired. Before a matching qualification
exists—or after any of those route inputs changes—the synthesized profile retains the built-in
route's declared coding capabilities and current behavior. The Computer cockpit's **Verify model for
this session** remains a separate, process-local proposal-authority check. Neither qualification path
executes its simulator proposal.

The Computer probe never executes the proposed action, reads a workspace, captures a real window,
or stores provider credentials in its report. It sends only repository-defined simulator metadata
and fixed synthetic strings. Changing the endpoint or qualification schema immediately resets
measured Computer authority to `none`.

Effort is omitted by default for compatible models. Add only values documented
for that exact provider/model pair. Unsupported explicit values fail before a
request is sent, and Grok-specific effort fields and headers are never sent to
compatible profiles.

## Environment-managed profiles

The native host creates read-only profiles only from paired variables:

| Profile | Endpoint | Credential |
| --- | --- | --- |
| `env-grokptah` | `GROKPTAH_API_BASE` | `GROKPTAH_API_KEY` |
| `env-openai` | `OPENAI_BASE_URL` or `OPENAI_API_BASE` | `OPENAI_API_KEY` |

Unset the endpoint variable to remove an environment-managed profile. The host
does not combine one profile's endpoint with another profile's credential.

## Compatibility behavior

- Redirects are refused on authenticated provider requests.
- Remote provider URLs require HTTPS; plaintext HTTP is loopback-only.
- Streaming buffers are bounded and preserve UTF-8 across arbitrary packet
  boundaries.
- Fragmented tool calls execute only after a complete terminal response and
  valid JSON-object arguments.
- Stalled responses cancel promptly, and streams that disconnect before their
  completion marker fail instead of returning partial output as complete.
- Model catalogs are bounded to one MiB while downloading; transport errors
  omit the provider hostname and discovered inventory.
- Gateways that reject `tool_choice` receive one bounded retry without it.
- Qualification retries transient rate limits and server errors at most three
  times.
- xAI qualification proactively refreshes near-expiry OIDC credentials and permits only one forced
  refresh after an HTTP 401 across the bounded probe suite. A refresh that changes provider,
  credential mode, or durable principal fails the qualification instead of continuing on a stale
  route. Compatible-provider behavior is unchanged.
- Model-list capability projection resolves the current xAI credential route once from the same live
  environment, keychain, or Grok Build session source used by execution; cached UI auth state is not
  an authority source.
- Malformed existing profile configuration is never silently replaced.

The built-in xAI profile retains its existing API-key and Grok Build OIDC
refresh/header behavior.
