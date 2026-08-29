# Semantic Help

Help answers "what does the shipped documentation say", offline, and cites what
it returns. It is not Chat: it has no tools, no conversation history, no
workspace, and no second route, and the projection a renderer receives is built
so it cannot acquire any of them.

Retrieval **is** the product. Asking a model for a written answer is a separate,
optional action, and it is **disabled in this build** — the host's provider seam
refuses every request, so no byte leaves the machine on Help's behalf.

## Where each decision lives

| Layer | Owns |
| --- | --- |
| `crates/common/grokptah-help-contract` | The corpus, every DTO, the digest rules, and the codegen that emits the shared artifacts |
| `crates/common/grokptah-help-authority` | Host-derived permissions, host-owned manifests, action-time reauthorization, host-side corpus filtering |
| `crates/common/grokptah-help-runtime` | The bounded executor, fail-closed redaction, reply validation against corpus bytes |
| `desktop/src-tauri/src/help.rs` | Tauri commands, the session table, the clock, and the disabled provider seam |
| `desktop/src/lib/help/` | Offline retrieval, digest and projection verification, the public surface |
| `desktop/src/lib/help/view.ts` | What a surface is allowed to *say* about a retrieval |
| `desktop/src/components/HelpCenter.tsx` | Focus, keyboard, live regions, layout |

Rust is the single owner of the contract. `help-codegen` emits the canonical
corpus, the public corpus, the TypeScript contract, the JSON Schema, and the
digest-parity artifact; `--verify` re-emits every one and compares byte for
byte, so a hand-edited artifact fails a gate instead of becoming a second
source of truth.

```
cargo run -p grokptah-help-contract --bin help-codegen -- --verify
```

## What the host decides, and what the renderer cannot

The renderer's entire reach is six commands, and each takes only an opaque
session token, an opaque ask handle, a question, and a locale. A grant,
admission, manifest, principal, capability, or route cannot arrive over IPC:
the contract's corresponding Rust types are `Serialize`-only, so there is no
command that could accept one.

Filtering happens in the host. `Authority::visible_corpus` returns the corpus a
principal is entitled to, and *that* is what crosses the boundary — a renderer
handed the whole corpus and asked to hide part of it would be holding the
content it is meant not to have. Record digests survive filtering so a citation
still verifies; the corpus-level digest is recomputed, because a filtered view
is honestly a different document.

## Presentation states

`grokptah.help-view.v1` flattens the retriever's outcome into exactly one
status, so a surface cannot render two at once or fall through to a default
that reads as an answer.

| Status | Meaning |
| --- | --- |
| `browse` | No question yet. Not a failure. |
| `answer` | One decisive leader. **The only status carrying an answer.** |
| `ambiguous` | Two or more articles too close to call. Candidates only. |
| `low-confidence` | Something matched; nothing well enough to lead. |
| `no-match` | Nothing in the corpus mentioned the question. |
| `rejected` | The query failed a bound and was never searched. |

`no-match` and `low-confidence` are kept apart deliberately: "we do not document
this" and "we document something adjacent, weakly" are different answers to a
reader, and collapsing them makes the honest one unavailable.

Every quote is re-read from the corpus before it is shown, and a source the
corpus cannot produce is dropped and counted, so the surface discloses the gap
rather than quietly showing less.

## Accessibility

- Search is a combobox over a listbox: `aria-expanded`, `aria-controls`,
  `aria-autocomplete="list"`, and `aria-activedescendant`. Options are not tab
  stops, so focus stays in the field the reader is typing in.
- Up/Down move the active option and wrap; Home/End jump; Enter opens. Escape
  belongs to the dialog, so a keyboard user is never stranded in the search box.
- The article pane is a tab stop: it scrolls, and at narrow widths it holds no
  focusable child, so without one a keyboard-only reader could not scroll it.
- Focus moves to the search field on open and returns to the opener on close;
  Tab is trapped; the background is `inert` and `aria-hidden`, except consent
  layers, which stay reachable above Help.
- Colours are existing theme tokens checked at AA against the panel. State is
  never carried by colour alone. `prefers-contrast: more` promotes secondary
  text to full strength; `forced-colors: active` maps borders and focus rings to
  system colours, where the words in each banner are all a reader has left.

## What Help does not claim

- **No embeddings and no online AI.** Retrieval fuses BM25 over terms with
  trigram cosine. Both run in-process against a corpus compiled into the build.
- **No restart durability.** Host Help state — authority, executor, session
  table — is in memory for the process lifetime and is gone on restart. Durable
  Help runs belong on main's canonical durable-run interfaces; until they are
  wired, this is stated rather than implied.
- **No provider, model, cost, or latency.** Nothing in this path observes any of
  them, so all four render as `unknown` rather than as a default a reader would
  mistake for a measurement.
- **A rank signal is not a certification**, and every percentage on screen says
  so next to itself.

## Gates

```
cargo run -p grokptah-help-contract --bin help-codegen -- --verify   # byte-exact codegen
cargo test -p grokptah-help-contract -p grokptah-help-authority -p grokptah-help-runtime
cd desktop && npm test && npm run typecheck && npm run build
cd desktop && npm run verify:help-public                            # public projection
cd desktop && npx vite build --config evidence/vite.config.ts \
           && node scripts/help-evidence.mjs                        # a11y audit + captures
```

`verify:help-public` reads artifacts rather than source: the emitted public
corpus, and the built renderer bundle, must contain no restricted chunk text or
restricted source id. `help-evidence.mjs` renders all six states at desktop and
narrow widths against the **synthetic fixture corpus** and runs axe-core over
each; captures and the report land in `desktop/evidence-out/`.

Fixtures are synthetic on purpose. A UI test written against real articles fails
for two unrelated causes — the component changed, or the documentation did — and
one edited sentence can move a score across a threshold.
