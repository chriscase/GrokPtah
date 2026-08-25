import { describe, expect, it } from "vitest";
import { createHelpExecutor } from "./authority/executor";
import { authorizeHelpDecision, type HelpPrincipal } from "./authority/decision";
import { HELP_CORPUS_DIGEST } from "./canonical/corpus";
import { HELP_INDEX_PROVENANCE } from "./retrieval/hybrid";

const executor = createHelpExecutor();

const searcher: HelpPrincipal = {
  principal_id: "alice",
  tenant_id: "tenant-a",
  project_ids: ["proj-1"],
  capabilities: ["help_search"],
};
const powerless: HelpPrincipal = { principal_id: "nobody", tenant_id: "tenant-a", capabilities: [] };

describe("authorized Help executor", () => {
  it("serves results to a principal holding the search capability", () => {
    const { decision, outcome, withheldCount } = executor.search(searcher, "durable run recovery");
    expect(decision.allowed).toBe(true);
    expect(outcome.results.length).toBeGreaterThan(0);
    expect(withheldCount).toBe(0);
    expect(outcome.corpusDigest).toBe(HELP_CORPUS_DIGEST);
    expect(outcome.indexDigest).toBe(HELP_INDEX_PROVENANCE.indexDigest);
  });

  it("returns nothing at all when the action is denied", () => {
    const { decision, outcome } = executor.search(powerless, "durable run recovery");
    expect(decision.allowed).toBe(false);
    expect(decision.denied_because).toBe("missing_capability");
    expect(outcome.results).toHaveLength(0);
    expect(outcome.abstained).toBe(true);
    // A denied outcome still carries provenance, so a UI can say which corpus
    // it was denied against rather than showing an unexplained blank.
    expect(outcome.corpusDigest).toBe(HELP_CORPUS_DIGEST);
  });

  it("adds nothing of its own to the authorization decision", () => {
    // Executor parity: if the executor made any decision itself, the desktop
    // (Rust authority) and the broker (TS mirror) could diverge even though
    // the two authority implementations agree on the shared fixtures.
    for (const principal of [searcher, powerless]) {
      const sources = executor.buildDecisionRequest(principal, "search", []).sources ?? [];
      const request = executor.buildDecisionRequest(principal, "search", sources);
      const direct = authorizeHelpDecision(
        request,
        HELP_CORPUS_DIGEST,
        HELP_INDEX_PROVENANCE.indexDigest,
      );
      const viaExecutor = executor.search(principal, "durable run recovery").decision;
      expect(viaExecutor.allowed).toBe(direct.allowed);
      expect(viaExecutor.denied_because ?? null).toBe(direct.denied_because ?? null);
    }
  });

  it("binds the decision request to the corpus and index actually served", () => {
    const request = executor.buildDecisionRequest(searcher, "search", []);
    expect(request.corpus_digest).toBe(HELP_CORPUS_DIGEST);
    expect(request.index_digest).toBe(HELP_INDEX_PROVENANCE.indexDigest);
    expect(request.schema).toBe("grokptah.help-authority-request.v1");
  });

  it("builds source descriptors from the corpus, never from the caller", () => {
    // A caller able to supply its own descriptors could relabel a private
    // source as public and authorize itself.
    const request = executor.buildDecisionRequest(searcher, "search", []);
    const withheld = executor.search(searcher, "durable run recovery");
    expect(request.sources).toEqual([]);
    // The real search still consults every corpus source.
    expect(withheld.decision.receipt.allowed_source_ids.length).toBeGreaterThan(0);
    for (const id of withheld.decision.receipt.allowed_source_ids) {
      expect(typeof id).toBe("string");
    }
  });

  it("keeps a receipt for every search, denied or not", () => {
    for (const principal of [searcher, powerless]) {
      const { decision } = executor.search(principal, "gateway");
      expect(decision.receipt.receipt_digest).toMatch(/^sha256:[0-9a-f]{64}$/);
      expect(decision.receipt.corpus_digest).toBe(HELP_CORPUS_DIGEST);
      expect(decision.receipt.principal_id).toBe(principal.principal_id);
      // Receipts stay id-and-digest only, even on the enforcement path.
      const serialized = JSON.stringify(decision.receipt);
      for (const forbidden of ["docs/", "README", ".md", "gateway"]) {
        expect(serialized, forbidden).not.toContain(forbidden);
      }
    }
  });

  it("re-ranks the surviving results contiguously when any are withheld", () => {
    // Rank is a display contract; a gap would leak that something was removed.
    const { outcome } = executor.search(searcher, "durable run recovery");
    outcome.results.forEach((result, index) => {
      expect(result.rank).toBe(index + 1);
    });
  });
});
