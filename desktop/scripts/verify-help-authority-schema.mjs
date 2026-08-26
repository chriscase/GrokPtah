/**
 * Conformance gate for the single Help authority schema and both language
 * parsers. The samples are built from the shipped canonical corpus; no
 * provider, network, session, or workspace is contacted.
 */
import { readFile } from "node:fs/promises";

import Ajv from "ajv/dist/2020.js";

const schema = JSON.parse(
  await readFile(new URL("../../docs/schemas/grokptah-help-authority.v1.schema.json", import.meta.url), "utf8"),
);
const {
  HELP_AUTHORITY_SCHEMA,
  buildHelpAuthorityRequest,
  createHelpAuthorityCleanupReceipt,
  parseHelpAuthorityRequest,
  parseHelpAuthorityResponse,
  validateHelpAuthorityRequest,
  validateHelpAuthorityResponse,
} = await import("../src/lib/help/authority/index.ts");
const { searchHelpCorpus } = await import("../src/lib/help/retrieval/hybrid.ts");
const { sha256Hex } = await import("../src/lib/help/canonical/digest.ts");

const ajv = new Ajv({ strict: true, validateFormats: false });
const validateSchema = ajv.compile(schema);
const results = searchHelpCorpus("durable run recovery", { limit: 2 }).results;
const request = buildHelpAuthorityRequest({
  requestId: "schema-conformance-request",
  query: "durable run recovery",
  results,
  provider: {
    profile: "schema-profile",
    tenant: "schema-tenant",
    model: "schema-model",
    routeRevision: "schema-route-1",
    dialect: "broker_native",
  },
  maxDurationMs: 1_000,
  deadlineAt: new Date(Date.now() + 1_000).toISOString(),
});
const context = request.context[0];
const source = context.sourceBindings[0];
const answer = context.text;
const citation = {
  citationId: "schema-citation-1",
  chunkId: context.chunkId,
  articleId: context.articleId,
  spanStart: 0,
  spanEnd: context.spanEnd,
  quotedText: context.text,
  quotedTextHash: `sha256:${sha256Hex(context.text)}`,
  sourceId: source.sourceId,
  sourceSectionDigest: source.sourceSectionDigest,
  claimIds: ["schema-claim-1"],
};
const response = {
  schema: HELP_AUTHORITY_SCHEMA,
  kind: "response",
  requestId: request.requestId,
  identity: request.identity,
  provider: request.provider,
  deadline: request.deadline,
  answer,
  claims: [{
    claimId: "schema-claim-1",
    text: answer,
    spanStart: 0,
    spanEnd: context.spanEnd,
    citationIds: ["schema-citation-1"],
  }],
  citations: [citation],
  uncertainty: "Only the quoted Help bytes support this answer.",
  cleanup: createHelpAuthorityCleanupReceipt(
    request.requestId,
    "finalized",
    "joined",
    false,
    "released",
  ),
};

const failures = [];
for (const [name, value] of [["request", request], ["response", response], ["cleanup", response.cleanup]]) {
  if (!validateSchema(value)) failures.push(`${name}: ${ajv.errorsText(validateSchema.errors)}`);
}
if (!parseHelpAuthorityRequest(request)) failures.push("request: TypeScript parser rejected schema sample");
if (!parseHelpAuthorityResponse(response, request)) failures.push("response: TypeScript parser rejected schema sample");
if (!validateHelpAuthorityRequest(request).accepted) failures.push("request: authority validator rejected schema sample");
if (!validateHelpAuthorityResponse(response, request).accepted) failures.push("response: authority validator rejected schema sample");

const unknownNested = {
  ...request,
  provider: { ...request.provider, secret: "must-reject" },
};
if (validateSchema(unknownNested)) failures.push("schema accepted an unknown nested provider key");
if (parseHelpAuthorityRequest(unknownNested)) failures.push("TypeScript parser accepted an unknown nested provider key");

if (schema.$defs?.request?.additionalProperties !== false ||
    schema.$defs?.response?.additionalProperties !== false ||
    schema.$defs?.cleanupReceipt?.additionalProperties !== false) {
  failures.push("top-level message definitions are not deny-unknown");
}

if (failures.length > 0) {
  console.error("Help authority schema conformance FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(`Help authority schema verified: ${HELP_AUTHORITY_SCHEMA}`);
console.log("  request: strict JSON Schema + TypeScript DTO/parser");
console.log("  response: strict JSON Schema + TypeScript DTO/parser");
console.log("  cleanup: strict JSON Schema + typed finalization receipt");
console.log("  nested unknown-key rejection: verified");
