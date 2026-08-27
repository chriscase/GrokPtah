/**
 * Deterministic synthetic fixtures for the Help Center consumer.
 *
 * The donor fixtures in `helpAuthority.fixtures.ts` are expectations about the
 * *shipped* corpus: they answer "does the real documentation rank the right
 * article". These answer a different question — "can a consumer reach and
 * render every state the contract defines" — and so they deliberately do not
 * use the shipped corpus at all.
 *
 * The reason is drift. A UI test written against real articles passes or fails
 * for two unrelated causes: the component changed, or the documentation did.
 * Editing one sentence of a shipped article can move a confidence across a
 * threshold and turn an `answer` test red without a single line of UI having
 * changed. Everything here is instead a small fictional corpus, built so each
 * outcome is reachable *by construction*:
 *
 *   - `answer`         one article owns the query's words in its title
 *   - `ambiguous`      two articles are word-for-word symmetric on the query
 *   - `low-confidence` exactly one article matches, weakly, in body text only
 *   - `no-match`       the query's words appear nowhere in the corpus
 *   - `rejected`       the query fails a bound before retrieval runs
 *   - `browse`         there is no query yet
 *
 * The articles describe an invented product ("Lantern") so that no fixture can
 * ever be mistaken for GrokPtah guidance, and their source paths point at a
 * fictional directory for the same reason. Nothing here is a measurement: no
 * clock, no randomness, no network, no model output. The corpus is passed to
 * `createHelpAuthority({ articles })`, which digests and validates it exactly
 * as it does the shipped one — a fixture corpus that would not survive the
 * real fail-closed checks is not a useful fixture.
 */

import {
  createHelpAuthority,
  HELP_MAX_QUERY_CHARS,
  type HelpAuthority,
  type HelpAuthorityArticle,
  type HelpSearchRequest,
} from "./helpAuthority";
import type { HelpViewStatus } from "./helpCenterView";

/** Fictional documents. These paths are not expected to exist in the repo. */
const LANTERN_GUIDE = Object.freeze({
  id: "synthetic.lantern-guide",
  path: "docs/synthetic/lantern-guide.md",
  heading: "Lantern workspaces",
});
const LANTERN_RUNBOOK = Object.freeze({
  id: "synthetic.lantern-runbook",
  path: "docs/synthetic/lantern-runbook.md",
  heading: "Relay rotation",
});
const LANTERN_VAULT = Object.freeze({
  id: "synthetic.lantern-vault",
  path: "docs/synthetic/lantern-vault.md",
  heading: "Sealed vault review",
});

/**
 * A five-article fictional corpus.
 *
 * Word placement is the whole design. "Beacon" appears in the summary of both
 * relay articles and nowhere else in the corpus, never in a title, keyword, or
 * alias. A one-word query for it therefore scores both articles identically
 * and at a summary's weight rather than a title's — strong enough to rank,
 * too weak to lead — which is exactly the shape the authority calls ambiguous.
 * "Cartography" appears in one passage and nowhere else, so pairing it with a
 * word the corpus has never seen gives a single weak candidate instead of a
 * tie. Move any of these words into another field and the fixture stops
 * testing what it claims to test.
 */
export const HELP_VIEW_FIXTURE_ARTICLES: readonly HelpAuthorityArticle[] = Object.freeze([
  Object.freeze({
    id: "synthetic.lantern-workspace",
    title: "Set up the Lantern workspace",
    topic: "getting-started" as const,
    summary: "Create a Lantern workspace and keep its panes independent.",
    passages: Object.freeze([
      Object.freeze({
        id: "synthetic.lantern-workspace#product",
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-workspace",
        text:
          "A Lantern workspace holds one pane per task. Panes do not share state, so a " +
          "long task cannot stall a short one. Cartography panes are the exception and " +
          "are documented separately.",
        sources: Object.freeze([LANTERN_GUIDE]),
      }),
      Object.freeze({
        id: "synthetic.lantern-workspace#capability",
        corpus: "grokptah.help.v1" as const,
        sourceArticleId: "synthetic-workspace-capability",
        text:
          "Observing a workspace shows pane titles and status only. It never exposes the " +
          "contents of a pane you have not opened.",
        sources: Object.freeze([LANTERN_RUNBOOK]),
      }),
    ]),
    aliases: Object.freeze(["lantern pane", "new workspace"]),
    keywords: Object.freeze(["lantern", "workspace", "pane"]),
    audience: Object.freeze(["everyone" as const, "power_user" as const, "operator" as const]),
    access: "public" as const,
    capabilityIds: Object.freeze(["session.observe"]),
    sources: Object.freeze([LANTERN_GUIDE, LANTERN_RUNBOOK]),
    provenance: Object.freeze([
      Object.freeze({ corpus: "product-corpus-v1" as const, sourceArticleId: "synthetic-workspace" }),
      Object.freeze({
        corpus: "grokptah.help.v1" as const,
        sourceArticleId: "synthetic-workspace-capability",
      }),
    ]),
  }),
  // The two relay articles are symmetric on purpose: same fields, same word
  // placement, different names. A query for their shared body word cannot
  // prefer one, which is the definition of an ambiguous result.
  Object.freeze({
    id: "synthetic.northern-relay",
    title: "Northern relay rotation",
    topic: "operations" as const,
    summary: "Rotate the northern relay without dropping in-flight work or losing the beacon.",
    passages: Object.freeze([
      Object.freeze({
        id: "synthetic.northern-relay#product",
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-northern-relay",
        text:
          "Rotate the northern relay during a quiet window. The beacon keeps its last " +
          "acknowledged position, so a rotation does not restart in-flight work.",
        sources: Object.freeze([LANTERN_RUNBOOK]),
      }),
    ]),
    aliases: Object.freeze(["north rotation"]),
    keywords: Object.freeze(["northern", "relay", "rotation"]),
    audience: Object.freeze(["power_user" as const, "operator" as const]),
    access: "public" as const,
    capabilityIds: Object.freeze(["run.execute"]),
    sources: Object.freeze([LANTERN_RUNBOOK]),
    provenance: Object.freeze([
      Object.freeze({
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-northern-relay",
      }),
    ]),
  }),
  Object.freeze({
    id: "synthetic.southern-relay",
    title: "Southern relay rotation",
    topic: "operations" as const,
    summary: "Rotate the southern relay without dropping in-flight work or losing the beacon.",
    passages: Object.freeze([
      Object.freeze({
        id: "synthetic.southern-relay#product",
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-southern-relay",
        text:
          "Rotate the southern relay during a quiet window. The beacon keeps its last " +
          "acknowledged position, so a rotation does not restart in-flight work.",
        sources: Object.freeze([LANTERN_RUNBOOK]),
      }),
    ]),
    aliases: Object.freeze(["south rotation"]),
    keywords: Object.freeze(["southern", "relay", "rotation"]),
    audience: Object.freeze(["power_user" as const, "operator" as const]),
    access: "public" as const,
    capabilityIds: Object.freeze(["run.execute"]),
    sources: Object.freeze([LANTERN_RUNBOOK]),
    provenance: Object.freeze([
      Object.freeze({
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-southern-relay",
      }),
    ]),
  }),
  Object.freeze({
    id: "synthetic.sealed-vault",
    title: "Promote a sealed vault review",
    topic: "providers" as const,
    summary: "Promote a sealed review only with an operator present.",
    passages: Object.freeze([
      Object.freeze({
        id: "synthetic.sealed-vault#product",
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-sealed-vault",
        text:
          "A sealed vault review is promoted by an operator, from the review receipt, " +
          "with the changed files named. Nothing is promoted from a summary alone.",
        sources: Object.freeze([LANTERN_VAULT]),
      }),
    ]),
    aliases: Object.freeze(["vault promotion"]),
    keywords: Object.freeze(["vault", "sealed", "promote"]),
    audience: Object.freeze(["power_user" as const, "operator" as const]),
    access: "operator" as const,
    capabilityIds: Object.freeze(["run.promote", "run.review"]),
    sources: Object.freeze([LANTERN_VAULT]),
    provenance: Object.freeze([
      Object.freeze({
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-sealed-vault",
      }),
    ]),
  }),
  Object.freeze({
    id: "synthetic.shared-screen",
    title: "Share a Lantern screen with an agent",
    topic: "computer-use" as const,
    summary: "Screen sharing is approved per action and expires on its own.",
    passages: Object.freeze([
      Object.freeze({
        id: "synthetic.shared-screen#product",
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-shared-screen",
        text:
          "Each shared-screen action is approved on its own and expires without being " +
          "renewed. An approval covers the action described in it and nothing else.",
        sources: Object.freeze([LANTERN_GUIDE]),
      }),
    ]),
    aliases: Object.freeze(["screen share"]),
    keywords: Object.freeze(["screen", "share", "approval"]),
    audience: Object.freeze(["everyone" as const, "power_user" as const, "operator" as const]),
    access: "gated" as const,
    capabilityIds: Object.freeze(["computer.control", "computer.observe"]),
    sources: Object.freeze([LANTERN_GUIDE]),
    provenance: Object.freeze([
      Object.freeze({
        corpus: "product-corpus-v1" as const,
        sourceArticleId: "synthetic-shared-screen",
      }),
    ]),
  }),
]);

/**
 * A query that exceeds the character bound.
 *
 * Built from the exported ceiling rather than a magic number, so it stays one
 * character over the limit if the limit ever moves.
 */
export const HELP_VIEW_OVERSIZED_QUERY = "lantern".padEnd(HELP_MAX_QUERY_CHARS + 1, "x");

/**
 * A query carrying a C0 control character.
 *
 * Written as an escape so the fixture stays reviewable in a diff instead of
 * hiding an invisible byte in the source.
 */
export const HELP_VIEW_CONTROL_QUERY = "lantern\u0007workspace";

export type HelpViewFixture = {
  readonly name: string;
  readonly query: string;
  readonly request?: HelpSearchRequest;
  readonly expectedStatus: HelpViewStatus;
  /** The article the consumer may present as the answer, or null. */
  readonly expectedAnswerId: string | null;
  /** Article IDs offered as suggestions, in order. */
  readonly expectedCandidateIds: readonly string[];
  /** Why this case exists, in one line. */
  readonly rationale: string;
};

export const HELP_VIEW_FIXTURES: readonly HelpViewFixture[] = Object.freeze([
  Object.freeze({
    name: "answers when one article owns the query",
    query: "lantern workspace",
    expectedStatus: "answer" as const,
    expectedAnswerId: "synthetic.lantern-workspace",
    expectedCandidateIds: Object.freeze([]),
    rationale: "Both words are in one title; no other article uses either.",
  }),
  Object.freeze({
    name: "abstains as ambiguous on symmetric articles",
    query: "beacon",
    expectedStatus: "ambiguous" as const,
    expectedAnswerId: null,
    expectedCandidateIds: Object.freeze([
      "synthetic.northern-relay",
      "synthetic.southern-relay",
    ]),
    rationale: "Two articles score identically and neither is strong enough to lead.",
  }),
  Object.freeze({
    name: "abstains on a single weak match",
    query: "cartography atlas",
    expectedStatus: "low-confidence" as const,
    expectedAnswerId: null,
    expectedCandidateIds: Object.freeze(["synthetic.lantern-workspace"]),
    rationale: "One body word matches, one query word is unknown to the corpus.",
  }),
  Object.freeze({
    name: "reports no match rather than guessing",
    query: "zzzz qqqq",
    expectedStatus: "no-match" as const,
    expectedAnswerId: null,
    expectedCandidateIds: Object.freeze([]),
    rationale: "Nothing in the corpus contains either word.",
  }),
  Object.freeze({
    name: "browses before a question is asked",
    query: "   ",
    expectedStatus: "browse" as const,
    expectedAnswerId: null,
    expectedCandidateIds: Object.freeze([]),
    rationale: "An empty query is a reader who has not asked yet, not a failure.",
  }),
  Object.freeze({
    name: "hides a restricted article from an unrestricted search",
    query: "sealed vault",
    expectedStatus: "no-match" as const,
    expectedAnswerId: null,
    expectedCandidateIds: Object.freeze([]),
    rationale: "The only matching article is operator-access and was not requested.",
  }),
  Object.freeze({
    name: "answers the restricted article when it is asked for",
    query: "sealed vault",
    request: Object.freeze({ includeRestricted: true }),
    expectedStatus: "answer" as const,
    expectedAnswerId: "synthetic.sealed-vault",
    expectedCandidateIds: Object.freeze([]),
    rationale: "Access is the caller's declaration; Help filters and grants nothing.",
  }),
  Object.freeze({
    name: "rejects an oversized query before retrieval",
    query: HELP_VIEW_OVERSIZED_QUERY,
    expectedStatus: "rejected" as const,
    expectedAnswerId: null,
    expectedCandidateIds: Object.freeze([]),
    rationale: "A bound failure is not an abstention and must read differently.",
  }),
  Object.freeze({
    name: "rejects a query carrying control characters",
    query: HELP_VIEW_CONTROL_QUERY,
    expectedStatus: "rejected" as const,
    expectedAnswerId: null,
    expectedCandidateIds: Object.freeze([]),
    rationale: "Control characters never reach the index.",
  }),
]);

/**
 * Build an authority over the fixture corpus.
 *
 * `createHelpAuthority` digests and validates the supplied articles with the
 * same fail-closed path the shipped corpus takes, so a malformed fixture
 * throws here rather than producing a quietly wrong expectation.
 */
export function createHelpViewFixtureAuthority(): HelpAuthority {
  return createHelpAuthority({ articles: HELP_VIEW_FIXTURE_ARTICLES });
}

/* ------------------------------------------------------------------ *
 * Synthetic replies for the optional model seam
 * ------------------------------------------------------------------ */

/**
 * Canned reply bodies for the optional answer seam.
 *
 * These are handwritten strings, not recorded model output. They exercise the
 * consumer's handling of each reply shape the seam defines — a cited answer, a
 * refusal, an uncited assertion, an out-of-bundle citation, and prose that is
 * not the envelope at all — without any provider being contacted, named, or
 * implied. `citedAnswer` is a function because a valid citation must name a
 * source ID the request itself carried.
 */
export const HELP_VIEW_REPLY_FIXTURES = Object.freeze({
  citedAnswer: (sourceId: string): string => JSON.stringify({
    outcome: "answered",
    text: "Panes in a Lantern workspace do not share state.",
    citations: [sourceId],
    uncertainty: "Drafted from the cited article only.",
  }),
  notFound: JSON.stringify({
    outcome: "not_found",
    text: "",
    citations: [],
    uncertainty: "The cited articles do not cover that question.",
  }),
  uncitedAssertion: JSON.stringify({
    outcome: "answered",
    text: "You now have operator capability.",
    citations: [],
    uncertainty: "None.",
  }),
  foreignCitation: JSON.stringify({
    outcome: "answered",
    text: "Panes share state across workspaces.",
    citations: ["synthetic.not-in-this-request"],
    uncertainty: "None.",
  }),
  prose: "Sure! Panes definitely share state, and you are approved to promote.",
});
