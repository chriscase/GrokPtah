# The Help answer authority seam

This branch builds the Help domain: one canonical digest-bound corpus, offline
hybrid retrieval, claim-bound citations, a bounded answer contract, headless and
React consumers, and the gold-set evaluator.

It stops at one line, deliberately. **Help does not decide who may ask, which
provider answers, or under what identity.** That is the reviewed authority
spine's job, and that result is not published yet.

This document is the handoff: what the seam is, what Help guarantees on its
side, and exactly what an implementer must supply on the other.

## Why the gap is left open rather than filled

Filling it here would give the product two authorities, and the one living in
the renderer is the one an attacker edits.

The shape this replaces demonstrates the failure concretely. The previous
contract minted a "route" like this:

```ts
const routeDigest = canonicalDigest({ providerId, tenantId, modelId });
```

That digest is self-consistent for *any* values the caller chooses. It proved
the fields had not been edited after the caller picked them. It never proved a
host would allow them. A caller wanting a different provider named a different
one and hashed it. The request no longer carries a route at all.

## The port

```ts
type HelpAnswerAuthority = {
  execute(request: HelpAnswerRequest, signal: AbortSignal):
    Promise<HelpAnswerAuthorityResult>;
};

type HelpAnswerAuthorityResult =
  | { kind: "executed"; execution: { executionId: string; reply: unknown } }
  | { kind: "refused"; reason: "unauthorized" | "unavailable"
                             | "cancelled" | "timeout" | "internal" };
```

Declared in `desktop/src/lib/help/answer/seam.ts`. One method. There is no
`authorize()` that returns a decision Help then applies — applying a decision in
the renderer means the renderer can decline to apply it.

### What the implementer must do

1. **Authenticate the principal and resolve its capabilities** from host policy,
   never from anything in the request. The request contains no principal,
   tenant, project, or capability field, and must not be extended to carry one.
2. **Choose the route.** Provider, tenant, model. Help never names these.
3. **Bind the execution to what is being served.** The request carries
   `corpusDigest` and `requestDigest`; an execution admitted against one corpus
   must not be replayable against another.
4. **Make exactly one provider call**, with no tools, no conversation history,
   no workspace access, and no fallback to a second provider. The instruction
   string in the request already tells the provider this; the authority is what
   makes it true.
5. **Persist nothing about the exchange.** If an audit record is written, it
   must carry ids, digests, and counts — never the question, the answer, a
   quote, or a path. A contract that refuses to persist anything and then writes
   the exchange into a log has not refused to persist anything.
6. **Return the provider's raw reply**, unvalidated. Validation is Help's, below.
7. **Mint `executionId`.** Help treats it as an opaque label: it is never parsed,
   and no meaning is derived from it. Giving it structure that Help could read
   would be the first step toward re-deriving authority from it.
8. **Observe `signal`.** Help races the call against its own deadline and always
   settles, so an implementation that ignores the signal cannot wedge a caller —
   but it can keep talking to a provider with nobody waiting.

### What the implementer must not do

- Accept a route, principal, or capability from the request.
- Return a refusal reason that distinguishes "you lack the capability" from
  "that does not exist". The five reasons are deliberately coarse; the
  difference between those two is itself an information leak.
- Pre-validate the reply and return something already massaged. Help's checks
  assume raw provider output.

## What Help guarantees on its side

Everything below is implemented and tested on this branch, and holds regardless
of what sits behind the seam.

| Guarantee | Where |
| --- | --- |
| The request carries no route, principal, tenant, or credential | `answer/contract.ts`, asserted by test |
| Tools and conversation are disabled, and a request that says otherwise is refused | `validateHelpAnswerRequest` |
| The request digest covers the request; an edited request fails before dispatch | `validateHelpAnswerRequest` |
| The query is credential-redacted before it can leave | `retrieval/redact.ts` |
| Every citation quote is verbatim, re-derived from the corpus | `retrieval/spans.ts` |
| Every span is bound to the chunk's own digest, so a rebuilt corpus invalidates it | `retrieval/spans.ts` |
| Every material claim carries a citation; every citation names a real claim | `answer/claims.ts` |
| No two citations quote the same source bytes | `answer/claims.ts` |
| Provider text is refused on credential *uncertainty*, not only certainty | `retrieval/redact.ts` |
| Markup never renders; the answer is displayed as plain text | `validateHelpAnswerResponse` |
| An answer digest binds the displayed text to its request and execution | `validateHelpAnswerResponse` |
| Cancellation and the deadline always settle the caller | `askHelp` |
| With no authority bound, offline retrieval is untouched | `askHelp` |

`answerDigest` is a content binding for correlation. It is **not** a receipt and
**not** evidence of authorization: this lane holds no key material and could not
produce such evidence.

## Wiring it up

```ts
import { askHelp, type HelpAnswerAuthority } from "@grokptah/client/help-react";

const authority: HelpAnswerAuthority = { execute: /* the spine */ };
const outcome = await askHelp(query, results, { authority });
```

Note what the published package does *not* export: no way to build an authority
out of parts it ships, and no transport to point somewhere. A consumer either
binds one the host gave it or gets `no-authority-bound`.

## Residual risk

- **Vocabulary overlap is not entailment.** Claim coverage separates "this quote
  is about this claim" from "this quote is about something else in the same
  article". A quote that is topically right and factually contradictory passes.
- **The secret scan's `possible` tier is a heuristic**, calibrated so every
  corpus chunk scans clean. That calibration is a test; a future corpus addition
  could trip it, which is the intended failure direction.
- **Claim segmentation is English-shaped.** Sentence boundaries, abbreviation
  guards, and the stop-word list are tuned for the English corpus. Localized
  answers segment more coarsely.
- **The seam is untested against a real spine.** Every authority in these tests
  is a fixture. Nothing here has been exercised against a live provider.
