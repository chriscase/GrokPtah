/**
 * Runtime boundary for bundled Help JSON.
 *
 * TypeScript declarations disappear at runtime. Importing JSON and casting it
 * to `HelpCorpus` therefore accepted missing fields, extra fields, invalid enum
 * values, and wrong scalar types even though Rust's serde boundary rejects the
 * same bytes. This parser validates the closed v1 corpus shape before the
 * digest verifier or any lookup observes it. It returns the original object,
 * so callers do not gain a second normalized representation of the corpus.
 */

import type {
  HelpArticle,
  HelpChunk,
  HelpChunkKind,
  HelpCorpus,
  HelpSourceAnchor,
  HelpTopic,
  HelpVisibility,
} from "../generated/contract";

type JsonObject = Record<string, unknown>;

/** The only corpus schema this parser understands. Unknown versions fail closed. */
export const HELP_CORPUS_SCHEMA_VERSION = "grokptah.help-canonical.v1" as const;

export class HelpCorpusSchemaError extends Error {
  constructor(readonly path: string, readonly problem: string) {
    super(`Invalid Help corpus at ${path}: ${problem}`);
    this.name = "HelpCorpusSchemaError";
  }
}

function exactObject(value: unknown, path: string, keys: readonly string[]): JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new HelpCorpusSchemaError(path, "expected an object");
  }
  const record = value as JsonObject;
  const expected = new Set(keys);
  for (const key of Object.keys(record)) {
    if (!expected.has(key)) throw new HelpCorpusSchemaError(`${path}.${key}`, "unknown field");
  }
  for (const key of keys) {
    if (!Object.hasOwn(record, key)) {
      throw new HelpCorpusSchemaError(`${path}.${key}`, "missing required field");
    }
  }
  return record;
}

function stringValue(value: unknown, path: string): string {
  if (typeof value !== "string") throw new HelpCorpusSchemaError(path, "expected a string");
  return value;
}

function enumValue<const T extends string>(
  value: unknown,
  path: string,
  allowed: readonly T[],
): T {
  const text = stringValue(value, path);
  if (!(allowed as readonly string[]).includes(text)) {
    throw new HelpCorpusSchemaError(path, `expected one of ${allowed.join(", ")}`);
  }
  return text as T;
}

function arrayValue(value: unknown, path: string): readonly unknown[] {
  if (!Array.isArray(value)) throw new HelpCorpusSchemaError(path, "expected an array");
  return value;
}

function stringArray(value: unknown, path: string): readonly string[] {
  return arrayValue(value, path).map((item, index) => stringValue(item, `${path}[${index}]`));
}

const VISIBILITIES = ["public", "gated", "operator"] as const satisfies readonly HelpVisibility[];
const CHUNK_KINDS = ["title", "summary", "body"] as const satisfies readonly HelpChunkKind[];
const TOPICS = [
  "getting-started",
  "providers",
  "computer-use",
  "operations",
] as const satisfies readonly HelpTopic[];

function sourceValue(value: unknown, path: string): HelpSourceAnchor {
  const source = exactObject(value, path, ["id", "path", "heading", "visibility", "digest"]);
  stringValue(source.id, `${path}.id`);
  stringValue(source.path, `${path}.path`);
  stringValue(source.heading, `${path}.heading`);
  enumValue(source.visibility, `${path}.visibility`, VISIBILITIES);
  stringValue(source.digest, `${path}.digest`);
  return value as HelpSourceAnchor;
}

function articleValue(value: unknown, path: string): HelpArticle {
  const article = exactObject(value, path, [
    "id",
    "title",
    "topic",
    "summary",
    "body",
    "aliases",
    "keywords",
    "source_ids",
    "visibility",
    "capability_ids",
    "digest",
  ]);
  stringValue(article.id, `${path}.id`);
  stringValue(article.title, `${path}.title`);
  enumValue(article.topic, `${path}.topic`, TOPICS);
  stringValue(article.summary, `${path}.summary`);
  stringValue(article.body, `${path}.body`);
  stringArray(article.aliases, `${path}.aliases`);
  stringArray(article.keywords, `${path}.keywords`);
  stringArray(article.source_ids, `${path}.source_ids`);
  enumValue(article.visibility, `${path}.visibility`, VISIBILITIES);
  stringArray(article.capability_ids, `${path}.capability_ids`);
  stringValue(article.digest, `${path}.digest`);
  return value as HelpArticle;
}

function chunkValue(value: unknown, path: string): HelpChunk {
  const chunk = exactObject(value, path, [
    "id",
    "article_id",
    "kind",
    "ordinal",
    "text",
    "locale",
    "source_ids",
    "visibility",
    "digest",
  ]);
  stringValue(chunk.id, `${path}.id`);
  stringValue(chunk.article_id, `${path}.article_id`);
  enumValue(chunk.kind, `${path}.kind`, CHUNK_KINDS);
  if (typeof chunk.ordinal !== "number" || !Number.isSafeInteger(chunk.ordinal) || chunk.ordinal < 0) {
    throw new HelpCorpusSchemaError(`${path}.ordinal`, "expected a non-negative safe integer");
  }
  stringValue(chunk.text, `${path}.text`);
  stringValue(chunk.locale, `${path}.locale`);
  stringArray(chunk.source_ids, `${path}.source_ids`);
  enumValue(chunk.visibility, `${path}.visibility`, VISIBILITIES);
  stringValue(chunk.digest, `${path}.digest`);
  return value as HelpChunk;
}

/** Validate and return an exact v1 Help corpus document. */
export function parseHelpCorpus(value: unknown): HelpCorpus {
  const corpus = exactObject(value, "$", [
    "schema_version",
    "content_version",
    "sources",
    "articles",
    "chunks",
    "digest",
    "source_digest",
  ]);
  const schemaVersion = stringValue(corpus.schema_version, "$.schema_version");
  if (schemaVersion !== HELP_CORPUS_SCHEMA_VERSION) {
    throw new HelpCorpusSchemaError(
      "$.schema_version",
      `unsupported schema version ${schemaVersion}`,
    );
  }
  stringValue(corpus.content_version, "$.content_version");
  arrayValue(corpus.sources, "$.sources").forEach((item, index) =>
    sourceValue(item, `$.sources[${index}]`),
  );
  arrayValue(corpus.articles, "$.articles").forEach((item, index) =>
    articleValue(item, `$.articles[${index}]`),
  );
  arrayValue(corpus.chunks, "$.chunks").forEach((item, index) =>
    chunkValue(item, `$.chunks[${index}]`),
  );
  stringValue(corpus.digest, "$.digest");
  stringValue(corpus.source_digest, "$.source_digest");
  return value as HelpCorpus;
}
