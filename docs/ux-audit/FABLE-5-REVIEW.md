# Claude / Fable 5 review record

This document preserves the outcome of the read-only design review performed
in Claude with Fable 5 after the Phase 1 product-research audit. It is a
distilled review record, not a transcript or a claim that Fable exercised
hosted production behavior.

## Review scope

Fable reviewed the current GrokPtah desktop direction, the Phase 1 audit, and
the proposed Agent/Lane model. No production code or repository state was
changed by the review.

## Overall assessment

The runtime and platform work is comparatively strong, but the visible product
still presents itself primarily as a session-first engineering console. The
next design effort should clarify the product model before adding more panels
or visual decoration.

## Highest-impact findings

1. Durable Agents are buried instead of presented as a primary, long-lived
   object.
2. Sessions, tabs, and the Live rail create overlapping navigation concepts.
3. The default cockpit is too dense for ordinary work; an expert Grid view
   should remain available without being the default starting point.
4. Error and empty states can appear simultaneously or expose contradictory
   meanings, weakening trust and obscuring the next action.
5. A panel can appear visually adjacent to the wrong Lane unless ownership is
   explicit at every contextual surface.
6. Scratch workspace names such as `.tmp*` add implementation noise and make
   work appear less understandable than it is.
7. Runtime target and connection state are not prominent enough for local,
   local-service, and hosted operation.
8. Destructive or lifecycle actions need clearer distinctions, especially
   Archive Lane versus Retire Agent.
9. Terminology and state presentation need a shared product grammar rather
   than component-specific wording.
10. The main application component is becoming a bottleneck and should be
    decomposed as the new information architecture is implemented.

## Recommended design sequence

1. Establish a shared error, empty, loading, disconnected, archived, and
   recovery state grammar.
2. Make the Agent/Lane relationship and active Lane scope visible throughout
   the product.
3. Reduce duplicate navigation and establish a clear Work/Agents/Lanes model.
4. Improve migration hygiene so user-facing workspace labels do not foreground
   scratch implementation details.
5. Define a concise glossary and lifecycle language.
6. Decompose the application shell while implementing the approved design in
   vertical slices.

## Evidence boundary

The review did not provide a substitute for the real-application walkthrough.
Hosted runtime behavior still requires an end-to-end product exercise, and the
Phase 1 screenshot index remains the authoritative visual baseline. The review
should therefore guide prototype priorities, while observed behavior and
repository contracts determine implementation acceptance.
