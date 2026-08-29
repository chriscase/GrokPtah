/**
 * A harness for capturing Help's rendered states.
 *
 * It mounts the real component against the synthetic fixture corpus, so the
 * screenshots and the accessibility audit are of shipped code and invented
 * content — no private data, no provider call, and no dependence on the real
 * documentation, which would make the evidence drift when a doc is edited.
 *
 * The state is chosen by `?state=` so one build serves every capture.
 */
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { HelpCenter } from "../src/components/HelpCenter";
import {
  HELP_VIEW_FIXTURE_CORPUS,
  HELP_VIEW_FIXTURE_QUERIES,
} from "../src/lib/help/view.fixtures";
import "../src/styles/app.css";

const QUERIES: Record<string, string> = {
  browse: HELP_VIEW_FIXTURE_QUERIES.browse,
  answer: HELP_VIEW_FIXTURE_QUERIES.answer,
  ambiguous: HELP_VIEW_FIXTURE_QUERIES.ambiguous,
  "low-confidence": HELP_VIEW_FIXTURE_QUERIES.lowConfidence,
  "no-match": HELP_VIEW_FIXTURE_QUERIES.noMatch,
  rejected: "x".repeat(600),
};

const state = new URLSearchParams(window.location.search).get("state") ?? "browse";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <HelpCenter
      open
      onClose={() => {}}
      corpus={HELP_VIEW_FIXTURE_CORPUS}
      initialQuery={QUERIES[state] ?? ""}
    />
  </StrictMode>,
);
