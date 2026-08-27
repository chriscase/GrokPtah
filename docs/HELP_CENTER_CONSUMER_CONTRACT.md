# Help Center consumer contract

`grokptah.help-center-view.v1` is the presentation layer above the Help
authority. It answers one question the headless contract deliberately does not:
**given what the retriever decided, what is a UI allowed to put on screen?**

This document does not restate `grokptah.help-authority.v1`. Retrieval,
abstention thresholds, citation spans, corpus validation, and the reply
envelope are specified once, in [`HELP_AUTHORITY_CONTRACT.md`](HELP_AUTHORITY_CONTRACT.md),
and nothing here overrides them. Read that first; this is the layer that
consumes it.

## Modules

| Module | Role |
| --- | --- |
| `desktop/src/lib/helpCenterView.ts` | Presentation states, labels, verified spans, seam wording |
| `desktop/src/lib/helpCenterView.fixtures.ts` | Synthetic corpus reaching every state by construction |
| `desktop/src/components/HelpCenter.tsx` | Desktop reference consumer: focus, keyboard, live regions, timeout |

`helpCenterView.ts` imports no React and performs no I/O, so an embedder with
its own visual language renders the same states GrokPtah's desktop renders.
It is published through `@grokptah/client` and `@grokptah/client/ui-core`.

## The three rules

Everything below follows from three properties, each enforced in code rather
than left to a component's discretion, and each covered by a test.

**An abstention is never an answer.** `HelpViewState.answer` is populated for
exactly one status. In every other status it is `null` and `canAskModel` is
`false`, mirroring `buildHelpAnswerRequest`'s own refusal to build a request
from a retrieval that already said it did not know. Candidates still exist and
may be offered — as suggestions, under a banner that says what they are.

**A citation is verified before it is shown.** Every span is re-resolved
through `authority.resolveSpan` and dropped if the corpus does not reproduce
the quote. Drops are counted in `unverifiedSpanCount` so a consumer discloses
the gap instead of quietly rendering fewer quotes.

**A documented capability is not a granted one.** Every capability label
carries `documented: true` and `liveAvailability: "unknown"`, and access labels
say what they do not confer. Availability, approval, lease, and quota are
checked elsewhere; a consumer must make its own live check before offering an
operation.

## States

One status is derived per retrieval, flattening the authority's
`(outcome, abstainReason, rejection)` triple so a UI cannot render two at once
or fall through to a default that reads as an answer.

| Status | From | What the consumer may present |
| --- | --- | --- |
| `browse` | `abstain` / `empty-query` | The filtered corpus. Not a failure: nobody has asked yet. |
| `answer` | `answer` | The leader, with verified spans. The only status carrying an answer. |
| `ambiguous` | `abstain` / `ambiguous` | Tied candidates, named as candidates. |
| `low-confidence` | `abstain` / `low-confidence` | Weak candidates, named as candidates. |
| `no-match` | `abstain` / `no-match` | Nothing. The corpus does not cover it. |
| `rejected` | `rejected` | The bound that failed, worded as a rejection, never as an abstention. |

An abstain reason this contract does not recognise degrades to
`low-confidence` — the weakest presentable state — never to `answer`.
`state.outcome`, `state.abstainReason`, and `state.rejection` are carried
through unchanged alongside the derived status, so a consumer can always report
what the retriever actually said rather than only this layer's wording of it.

`helpBrowseArticles` supplies the `browse` listing, since retrieval has no
"return everything" entry point. It applies the same three declarative filters
a `HelpSearchRequest` applies, in the same direction, and orders by topic then
article ID by code point; `helpCenterView.test.ts` asserts that agreement
against the authority rather than assuming it.

## Accessibility

The desktop consumer is the reference implementation of the behaviour an
embedder is expected to match.

- **Semantic search is a combobox.** The search field carries
  `role="combobox"`, `aria-expanded`, `aria-controls`, `aria-autocomplete="list"`,
  and `aria-activedescendant`; results are a `listbox` of `option`s. Options are
  not tab stops: focus stays in the field the reader is typing in, and the
  active option is announced through `aria-activedescendant`.
  *This is a change from the previous Help Center, where the field exposed the
  implicit `textbox` role. The accessible name, "Search help", is unchanged; a
  test querying it by role must query `combobox`.*
- **Keys.** Up/Down move the active option and wrap at both ends, Home/End jump
  to first/last, Enter opens the active option. Escape is left to the dialog —
  it dismisses the top confirmation, or closes Help — so a keyboard user is
  never stranded inside the search field.
- **Focus.** Opening moves focus to the search field and records the opener;
  Tab is trapped in the dialog, or in the top confirmation when one is open;
  each confirmation restores focus to its own opener, and closing Help restores
  the original one. The application background is marked `inert` and
  `aria-hidden`, except consent layers (`data-modal-layer="consent"`), which
  must stay reachable above Help.
- **Announcements.** The retrieval outcome is a labelled `status` region — an
  `alert` when the query was rejected — carrying the state headline, what it
  means, and the retriever's raw verdict.
- **Contrast.** Colours come from the existing theme tokens and are checked
  against `--bg-panel` at AA for body text. State is never carried by colour
  alone: every banner, badge, and capability chip also says its state in words.
  `prefers-contrast: more` promotes secondary text to full strength and makes
  every boundary visible; `forced-colors: active` maps borders and focus rings
  to system colours, where the words are all a reader has left.

## Loading and timeout truth

Retrieval is offline, synchronous, and pure: there is **no spinner and no
network for search**, and showing one would be a lie about where the answer
came from. The optional model seams are the only outbound paths.

- Nothing is sent without a confirmation naming what would be sent — for the
  cited answer seam, the exact articles and source IDs in the bundle.
- While a reply is outstanding, the UI says nothing has been received yet, and
  shows provider, model, cost, and latency as `unknown`, read from the
  request's own `unknowns` rather than composed for display.
- A caller-supplied provider label is presented as the route the embedder
  chose, marked *identity unverified*. It never becomes the identity of
  whatever actually answers, and never implies a model.
- `timeoutMs` is enforced, not merely declared: the request is abandoned and
  the adapter's `AbortSignal` is aborted. The timeout is reported against the
  **declared budget** ("declared a 20s budget"), and states that whether the
  request was ever served is unknown. No elapsed time is asserted, because this
  layer does not measure latency and a rounded guess would read as a
  measurement.
- A reader can cancel an outstanding request, which aborts it the same way.
- A malformed, uncited, or out-of-bundle reply is shown as *not shown*, with
  the reason. A well-formed `not_found` / `abstained` reply is shown as a
  refusal, not as a failure. In every case the cited documentation remains the
  authority.

Model, cost, and usage are **unknown** at this layer and are rendered as such.

## Fixtures

`helpCenterView.fixtures.ts` is a five-article fictional corpus ("Lantern"),
not the shipped one. UI tests written against real articles fail for two
unrelated causes — the component changed, or the documentation did — and one
edited sentence can move a confidence across a threshold. The fixture corpus
instead makes each outcome reachable *by construction*: one article owns a
query's words for `answer`; two are word-for-word symmetric for `ambiguous`;
one matches weakly in a single field for `low-confidence`; and the reply
fixtures are handwritten envelopes, never recorded model output.

It is still built through `createHelpAuthority({ articles })`, so it passes the
same validation and digest path the shipped corpus does. Pass an authority to
`HelpCenter` through the `authority` prop to serve any verified corpus.

## Backward compatibility

- `helpCenter.ts` is unedited. Every legacy export — `HELP_ARTICLES`,
  `searchHelp`, `buildHelpAssistantRequest`, `buildHelpSemanticRequest`, and
  both validators — keeps its behaviour and its published names.
- `HelpCenter`'s existing props (`onAskAssistant`, `onSearchSemantic`,
  `assistantProviderLabel`) still work. The legacy assistant seam calls the
  legacy builder and the legacy validator, so an existing embedder sees the
  same `grokptah.help-assistant-request.v1` payload it saw before.
- The new `onAnswer` prop is the cited seam over the canonical corpus. When it
  is supplied it replaces the legacy assistant seam in the UI; when it is not,
  the legacy seam is used unchanged.
- One legacy behaviour is deliberately narrowed: a provider ranking may reorder
  candidates but may no longer replace the result set, so it cannot turn an
  abstention into an answer or introduce an article retrieval did not find.

## Boundaries

- The consumer performs no network I/O of its own. It holds no credential,
  reads no workspace, session, or provider state, and grants no capability.
- Article prose is data. Nothing in a passage changes an article's declared
  access or capabilities, and the UI offers no operation on their behalf.
- `audience` and `includeRestricted` are the embedder's declarations about its
  own viewer. They filter what is listed and retrieved; they grant nothing, and
  a gated article shown under them is still gated.
