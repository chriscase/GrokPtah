# Embedding GrokPtah

GrokPtah capabilities can be embedded in another desktop application, service, or browser-backed
product without transferring host authority to that consumer. Consumers use versioned contracts,
opaque identifiers, and projections appropriate to their trust boundary.

## Choose the trust boundary first

A trusted native host may own credentials, filesystem access, provider routes, and Computer Use
authority. An untrusted browser or renderer receives only filtered projections and opaque handles.
Every operation is re-authorized server-side; a client-supplied principal, capability, route, raw
path, or grant is never treated as authority.

## Browser / War Room example

A browser or War Room UI connects through an authenticated broker. The broker owns provider
credentials, checks CSRF and idempotency, scopes calls to the user, team, workspace, and run, and
returns redacted status and evidence. Native Computer Use authority and unrestricted filesystem
paths do not cross into browser code.

## External cloud workers

An external coding worker starts from an exact repository and ref in isolated execution. Its
manager reports bounded, redacted lifecycle state and relative artifacts. A worker may prepare a
draft change, but review, approval, publication, and merge remain separate human-controlled steps.

## Headless UI primitives

Reusable UI consumers should compose transport-neutral types, offline retrieval, safe projections,
validation, and rendering helpers. Authority constructors, provider routing, private corpora,
credentials, and native-control transports remain host-only.
