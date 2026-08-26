import { readFile } from "node:fs/promises";

const bundlePath = new URL("../dist/public/grokptah-public.js", import.meta.url);
const uiCoreBundlePath = new URL("../dist/public/ui-core.js", import.meta.url);
const helpReactBundlePath = new URL("../dist/public/help-react.js", import.meta.url);
const bundle = await readFile(bundlePath, "utf8");
const uiCoreBundle = await readFile(uiCoreBundlePath, "utf8");
const helpReactBundle = await readFile(helpReactBundlePath, "utf8");
const manifest = JSON.parse(
  await readFile(new URL("../dist/public/package.json", import.meta.url), "utf8"),
);
if (
  manifest.name !== "@grokptah/client" ||
  manifest.exports?.["."]?.import !== "./grokptah-public.js" ||
  manifest.exports?.["."]?.types !== "./types/public.d.ts" ||
  manifest.exports?.["./ui-core"]?.import !== "./ui-core.js" ||
  manifest.exports?.["./ui-core"]?.types !== "./types/uiCore.d.ts" ||
  manifest.exports?.["./help-react"]?.import !== "./help-react.js" ||
  manifest.exports?.["./help-react"]?.types !== "./types/helpPublic.d.ts"
) {
  throw new Error("public package manifest does not expose the expected safe entry point");
}
const forbidden = [
  "@tauri-apps",
  "trusted.ts",
  "Authorization: Bearer",
  "XAI_API_KEY",
  "/Users/",
  "/private/",
  "GROKPTAH_HOME",
  "apiKey",
];
const leaked = forbidden.filter((needle) => bundle.includes(needle));
if (leaked.length > 0) {
  throw new Error(`public bundle contains forbidden authority markers: ${leaked.join(", ")}`);
}
const leakedUiCore = forbidden.filter((needle) => uiCoreBundle.includes(needle));
if (leakedUiCore.length > 0) {
  throw new Error(`ui-core bundle contains forbidden authority markers: ${leakedUiCore.join(", ")}`);
}
const leakedHelpReact = forbidden.filter((needle) => helpReactBundle.includes(needle));
if (leakedHelpReact.length > 0) {
  throw new Error(`help-react bundle contains forbidden authority markers: ${leakedHelpReact.join(", ")}`);
}
// The React entry must not inline React, and the dependency-free entries must
// not acquire a React dependency from the shared Help core.
if (/from\s*["']react["']/.test(uiCoreBundle) || /require\(["']react["']\)/.test(uiCoreBundle)) {
  throw new Error("ui-core bundle acquired a React dependency");
}
if (!/from\s*["']react/.test(helpReactBundle)) {
  throw new Error("help-react bundle does not treat React as an external peer");
}
// Nothing in the public surface may render provider or corpus text as HTML.
for (const [name, text] of [["public", bundle], ["ui-core", uiCoreBundle], ["help-react", helpReactBundle]]) {
  if (text.includes("dangerouslySetInnerHTML") || text.includes("innerHTML")) {
    throw new Error(`${name} bundle assigns raw HTML; Help excerpts must stay plain text`);
  }
}

const publicApi = await import(bundlePath.href);
const uiCoreApi = await import(uiCoreBundlePath.href);
const requiredExports = [
  "GrokPtahBrokerClient",
  "parseBrokerApproval",
  "parseBrokerEventUpdate",
  "parseBrokerRunProjection",
  "parseBrokerReviewProjection",
  "EXTERNAL_WORKER_CONTRACT",
  "parseExternalWorkerLaunchRequest",
  "parseExternalWorkerFollowUpRequest",
  "parseExternalWorkerLaunchResult",
  "parseExternalWorkerNotification",
  "applyExternalWorkerNotification",
  "searchHelp",
  "searchHelpArticles",
  "HELP_ARTICLES",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
  // Canonical Help core.
  "HELP_CORPUS",
  "HELP_CORPUS_DIGEST",
  "searchHelpCorpus",
  "createHelpSearchController",
  "describeHelpResultForAssistiveTech",
  "validateHelpAnswerResponse",
  "redactHelpText",
  "scanHelpForSecrets",
  "sanitizeHelpText",
  "verifyHelpModelChecksum",
  // Verification, provenance, and the task runtime.
  "buildHelpClaimSpan",
  "verifyHelpClaimSpan",
  "checkHelpClaimCoverage",
  "segmentHelpClaims",
  "HELP_INDEX_PROVENANCE",
  "createHelpTaskScheduler",
];
const missing = requiredExports.filter((name) => !(name in publicApi));
if (missing.length > 0) {
  throw new Error(`public bundle is missing required exports: ${missing.join(", ")}`);
}

// The published client may ask the server for a decision. It may not make one.
//
// Shipping `authorizeHelpDecision` and `createHelpExecutor` in a browser bundle
// let a consumer decide, in code it controls, whether it was allowed to see a
// source — a decision made by the party it constrains. Shipping
// `requestHelpAnswer` let it point the answer contract at any endpoint it liked
// from a bundle carrying GrokPtah's name. Neither is publishable.
const forbiddenExports = [
  "authorizeHelpDecision",
  "authorizeHelpDecisionJson",
  "parseHelpDecisionRequest",
  "createHelpExecutor",
  "HelpAuthorityMalformedError",
  "requestHelpAnswer",
  "buildHelpAnswerRequestCore",
  "sealHelpAnswerRequest",
  "helpAnswerRequestDigest",
  "validateHelpAnswerRequest",
];
for (const [name, api] of [
  ["public", publicApi],
  ["ui-core", uiCoreApi],
  ["help-react", await import(helpReactBundlePath.href)],
]) {
  const exposed = forbiddenExports.filter((symbol) => symbol in api);
  if (exposed.length > 0) {
    throw new Error(
      `${name} bundle exposes local Help authority or transport: ${exposed.join(", ")}`,
    );
  }
}

// The corpus ships in the bundle, so every source in it is published. A source
// that is not public must never reach a published corpus in the first place.
const nonPublicSources = publicApi.HELP_CORPUS.articles
  .flatMap((article) => article.sources ?? [])
  .filter((source) => source.visibility !== "public")
  .map((source) => `${source.id} (${source.visibility})`);
if (nonPublicSources.length > 0) {
  throw new Error(
    `published Help corpus contains non-public sources: ${nonPublicSources.join(", ")}`,
  );
}
for (const name of [
  "HELP_ARTICLES",
  "searchHelpArticles",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
  "EXTERNAL_WORKER_CONTRACT",
  "parseExternalWorkerFollowUpRequest",
  "parseExternalWorkerNotification",
  "applyExternalWorkerNotification",
]) {
  if (!(name in uiCoreApi)) throw new Error(`ui-core bundle is missing required export: ${name}`);
}
if ("GrokPtahBrokerClient" in uiCoreApi) {
  throw new Error("ui-core bundle must not expose the browser broker client");
}
if (!Object.isFrozen(publicApi.HELP_ARTICLES) || !Object.isFrozen(uiCoreApi.HELP_ARTICLES)) {
  throw new Error("published Help corpus must be immutable");
}
if (!Object.isFrozen(publicApi.HELP_ENTRIES) || !Object.isFrozen(uiCoreApi.HELP_ENTRIES)) {
  throw new Error("published capability-aware Help corpus must be immutable");
}

const helpHits = publicApi.searchHelp("restricted gateway", {
  audience: "operator",
  includeRestricted: true,
});
if (!helpHits.some(({ entry }) => entry.id === "enterprise-gateway-review")) {
  throw new Error("public Help Center search did not return the enterprise gateway entry");
}
const articleHits = publicApi.searchHelpArticles("restricted company gateway");
if (articleHits[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("public source-cited Help Center search did not rank the restricted gateway article");
}
const streamResult = publicApi.applyAssistantStreamChunk("", "consumer");
if (streamResult.kind !== "replace" || streamResult.text !== "consumer") {
  throw new Error("public stream helper did not apply a consumer update");
}
const broker = new publicApi.GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  fetcher: async () => new Response(null, { status: 204 }),
});
if (!(broker instanceof publicApi.GrokPtahBrokerClient)) {
  throw new Error("public broker client could not be constructed by a consumer");
}

// The Help core must retrieve, cite, and abstain identically from a consumer.
const helpOutcome = publicApi.searchHelpCorpus("why did my agent send the same request twice after a restart");
if (helpOutcome.results[0]?.articleId !== "operations.durable-recovery") {
  throw new Error("public Help retrieval did not rank the durable-recovery article for a paraphrase");
}
if (helpOutcome.corpusDigest !== publicApi.HELP_CORPUS_DIGEST) {
  throw new Error("public Help retrieval is not bound to the shipped corpus digest");
}
if (!helpOutcome.results[0].citations.every((citation) => citation.path && citation.heading)) {
  throw new Error("public Help retrieval returned a citation without a source anchor");
}
if (!publicApi.searchHelpCorpus("how do I bake sourdough bread").abstained) {
  throw new Error("public Help retrieval answered a question the corpus cannot support");
}
if (!publicApi.verifyHelpModelChecksum().ok) {
  throw new Error("published Help embedding model failed its checksum");
}
const redactedHelp = publicApi.searchHelpCorpus("my key xai-AbCdEf0123456789AbCdEf on the gateway");
if (JSON.stringify(redactedHelp).includes("AbCdEf")) {
  throw new Error("public Help retrieval echoed a credential from the query");
}
if (Object.isFrozen(publicApi.HELP_CORPUS) !== true) {
  throw new Error("published canonical Help corpus must be immutable");
}

const helpReactApi = await import(helpReactBundlePath.href);
for (const name of ["HelpResults", "HelpSearchInput", "HelpCitationList", "HelpHighlightedText", "useHelpSearch", "searchHelpCorpus", "HelpRoute"]) {
  if (!(name in helpReactApi)) throw new Error(`help-react bundle is missing required export: ${name}`);
}

// A consumer must be able to *verify*, and must not be able to authorize.
//
// The previous version of this file asserted the opposite: that a consumer
// could call `authorizeHelpDecision` and be denied by default. Denying by
// default is the right rule in the wrong place — running it inside the
// consumer's own bundle means the consumer chooses whether to run it.
const answerValidation = publicApi.validateHelpAnswerResponse(
  { schema: "grokptah.help-answer-response.v1" },
  {
    schema: "grokptah.help-answer-request.v1",
    corpusDigest: publicApi.HELP_CORPUS_DIGEST,
    indexDigest: publicApi.HELP_INDEX_PROVENANCE.indexDigest,
    context: [],
    admission: { admissionId: "sha256:none" },
  },
);
if (answerValidation.accepted !== false) {
  throw new Error("published response validation accepted a malformed reply");
}

// Claim coverage must be decidable by the consumer, over its own segmentation.
const coverage = publicApi.checkHelpClaimCoverage("Resume safely. Quota is separate.", []);
if (coverage.ok !== false || coverage.reason !== "uncovered-claim") {
  throw new Error("published claim coverage did not refuse an uncited answer");
}
if (publicApi.segmentHelpClaims("One. Two.").length !== 2) {
  throw new Error("published claim segmentation did not segment an answer");
}

// The secret scan must report uncertainty rather than clearing what it cannot
// rule out.
if (publicApi.scanHelpForSecrets("aGVsbG8gd29ybGQ=").confidence !== "possible") {
  throw new Error("published secret scan reported certainty it does not have");
}

// Claim spans must be re-verifiable by a consumer that did not produce them.
const spanChunk = publicApi.HELP_CORPUS.chunks[0];
const span = publicApi.buildHelpClaimSpan(spanChunk.id, spanChunk.text.slice(0, 16));
if (!span || publicApi.verifyHelpClaimSpan(span).ok !== true) {
  throw new Error("published claim spans did not verify against the published corpus");
}
if (publicApi.verifyHelpClaimSpan({ ...span, startUtf16: span.startUtf16 + 1 }).ok !== false) {
  throw new Error("published claim spans accepted a drifted offset");
}

// The index digest must bind the corpus actually shipped.
if (publicApi.HELP_INDEX_PROVENANCE.corpusDigest !== publicApi.HELP_CORPUS_DIGEST) {
  throw new Error("published index provenance is not bound to the published corpus");
}

// ---- packaged accessibility ------------------------------------------------
//
// Rendered from the *bundle*, not the source. A component whose accessibility
// is asserted only against `src/` is asserted against something no consumer
// installs: a bundler that drops an attribute, a minifier that mangles a
// generated id, or an entry that exports a different component all leave the
// source tests green.
//
// Effect-driven behaviour (background inerting, focus restoration) needs a DOM
// and is covered by `helpRoute.test.tsx`; what is checked here is the markup
// every consumer receives.
{
  const { JSDOM } = await import("jsdom");
  const dom = new JSDOM("<!doctype html><html><body><div id=\"app\"></div></body></html>", {
    pretendToBeVisual: true,
  });
  // `navigator` is a getter-only global on modern Node, so install these with
  // property descriptors rather than assignment, and restore the exact
  // descriptors afterwards.
  const installed = ["window", "document", "HTMLElement", "Node", "Element", "getComputedStyle", "navigator"];
  const priorDescriptors = new Map();
  for (const key of installed) {
    priorDescriptors.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
    Object.defineProperty(globalThis, key, {
      value: key === "window" ? dom.window : dom.window[key],
      configurable: true,
      writable: true,
    });
  }
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;

  try {
    const React = await import("react");
    const { createRoot } = await import("react-dom/client");
    const { act } = React;

    const container = dom.window.document.getElementById("app");
    const beside = dom.window.document.createElement("div");
    const root = createRoot(container);
    await act(async () => {
      root.render(
        React.createElement(
          React.Fragment,
          null,
          React.createElement("nav", { "data-testid": "chrome" }, "app chrome"),
          React.createElement(helpReactApi.HelpRoute, { open: true, onClose: () => {} }),
        ),
      );
    });
    beside.remove();

    const dialog = dom.window.document.querySelector('[role="dialog"]');
    if (!dialog) throw new Error("packaged Help route rendered no dialog");
    if (dialog.getAttribute("aria-modal") !== "true") {
      throw new Error("packaged Help dialog is not modal to assistive technology");
    }
    const labelledBy = dialog.getAttribute("aria-labelledby");
    if (!labelledBy || !dom.window.document.getElementById(labelledBy)) {
      throw new Error("packaged Help dialog has no resolvable accessible name");
    }
    if (!dom.window.document.querySelector('[aria-live="polite"]')) {
      throw new Error("packaged Help route has no polite live region for status");
    }
    const search = dom.window.document.querySelector("input");
    if (!search) throw new Error("packaged Help route rendered no search input");
    // A real `<label for>` is the preferred accessible name, so accept it
    // alongside the ARIA forms rather than demanding one particular mechanism.
    const named =
      search.getAttribute("aria-label") ||
      (search.getAttribute("aria-labelledby") &&
        dom.window.document.getElementById(search.getAttribute("aria-labelledby"))) ||
      (search.id && dom.window.document.querySelector(`label[for="${search.id}"]`));
    if (!named) throw new Error("packaged Help search input has no accessible name");
    // The palette must not inert itself: the route renders inside the app
    // container, so an ancestor carrying `inert` would take the dialog with it.
    for (let node = dialog.parentElement; node; node = node.parentElement) {
      if (node.hasAttribute("inert")) {
        throw new Error("packaged Help dialog is inside an inert ancestor");
      }
    }
    const chrome = dom.window.document.querySelector('[data-testid="chrome"]');
    if (!chrome?.hasAttribute("inert")) {
      throw new Error("packaged Help route did not make the background inert");
    }
    // Provider and corpus text is plain text, everywhere in the packaged tree.
    if (dom.window.document.body.querySelector("script")) {
      throw new Error("packaged Help route rendered a script element");
    }

    await act(async () => root.unmount());
    console.log("packaged accessibility verified: modal dialog, named, live region, inert background");
  } finally {
    for (const [key, descriptor] of priorDescriptors) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
    delete globalThis.IS_REACT_ACT_ENVIRONMENT;
    dom.window.close();
  }
}

console.log(`public bundle verified: ${requiredExports.join(", ")}`);
console.log("help-react entry verified: React externalized, primitives exported");
