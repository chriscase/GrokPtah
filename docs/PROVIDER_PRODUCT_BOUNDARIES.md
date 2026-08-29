# Provider product boundaries

GrokPtah integrates provider routes explicitly. A configured route is not evidence that a
particular product, quota, or credential was used; those claims require receipts from a named
live campaign. Local and offline qualification paths remain provider-free unless their evidence
states otherwise.

## Grok Build route

Grok Build is the coding and repository-work route. GrokPtah may use it only through an explicit
provider profile and the normal host authority, admission, and receipt boundaries. Source-only,
loopback, simulator, and offline-soak evidence must never be described as a live Grok Build run.

## Grok Bot boundary

Grok Bot is a separate xAI product. It is not a GrokPtah runtime dependency, it is not the manager
for GrokPtah agents, and GrokPtah must not consume Grok Bot quota unless a user deliberately
selects a separately configured integration for that purpose.
