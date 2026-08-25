# Provider and product boundaries

GrokPtah is provider-neutral at the interface level. A provider profile names
the route, tenant, model, credential class, and evidence required for a run;
the app must not imply that a configured route is live-certified or that it can
synchronize a provider account balance.

## Grok Build route

The built-in xAI profile can use the Grok Build OIDC/session route described in
[`PROVIDER_PROFILES.md`](./PROVIDER_PROFILES.md). A real Grok Build quota claim
requires an explicit live campaign and secret-free receipts. The local
Always-On soak uses a controlled loopback provider and therefore does not
contact Grok Build or consume its quota.

## Grok Bot boundary

Grok Bot is a separate xAI product. It is not a GrokPtah runtime dependency,
provider route, or development manager. GrokPtah must not silently invoke it,
spend its quota, or treat a Grok Bot result as Grok Build evidence. If a user
chooses to use Grok Bot independently, that is outside this application's
execution and certification surfaces.

## External development tools

Claude Code, Cursor, and a user-submitted Grok Build prompt may help develop or
review GrokPtah. Those tools are external to the shipped runtime. Their model
or quota usage must not be reported as a GrokPtah provider campaign unless the
campaign's own route and receipts prove that claim.
