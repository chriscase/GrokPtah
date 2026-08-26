import { HELP_CORPUS, HELP_CORPUS_DIGEST, getHelpSource } from "../canonical/corpus";
import { HELP_SOURCE_REGISTRY } from "../canonical/data";
import {
  HELP_SOURCE_BYTE_DIGEST,
  HELP_SOURCE_BYTE_IDENTITIES,
  type HelpSourceByteIdentity,
} from "../canonical/sourceManifest";
import { canonicalDigest } from "../canonical/digest";
import { HELP_MODEL_PROVENANCE } from "../model/artifact";

export { HELP_SOURCE_BYTE_DIGEST, HELP_SOURCE_BYTE_IDENTITIES };
export type { HelpSourceByteIdentity };

/**
 * Identity for the bytes behind a source anchor. The path and heading are
 * locators only; `digest` and `byteLength` are the evidence that gets bound
 * into an authority request and every citation.
 */
export type HelpSourceBinding = {
  readonly sourceId: string;
  readonly sourceSectionDigest: string;
  readonly sourceByteLength: number;
};

export type HelpAuthorityIdentity = {
  readonly corpusDigest: string;
  readonly sourceDigest: string;
  readonly modelDigest: string;
  readonly modelId: string;
  readonly modelVersion: string;
};

function sourceIdentity(sourceId: string): HelpSourceByteIdentity {
  const source = getHelpSource(sourceId);
  const identity = HELP_SOURCE_BYTE_IDENTITIES[sourceId];
  if (
    !source ||
    !identity ||
    identity.sourceId !== sourceId ||
    identity.path !== source.path ||
    identity.heading !== source.heading ||
    !Number.isSafeInteger(identity.byteLength) ||
    identity.byteLength < 1 ||
    !/^sha256:[0-9a-f]{64}$/u.test(identity.digest)
  ) {
    throw new Error(`help authority: source-byte identity is unavailable for ${sourceId}`);
  }
  return identity;
}

/** Return the source-byte binding or throw instead of falling back to names. */
export function getHelpSourceBinding(sourceId: string): HelpSourceBinding {
  const identity = sourceIdentity(sourceId);
  return Object.freeze({
    sourceId,
    sourceSectionDigest: identity.digest,
    sourceByteLength: identity.byteLength,
  });
}

/**
 * Bind the exact source bytes used by the canonical corpus. Sorting is
 * intentional: article/chunk ordering must not alter source identity.
 */
export function helpSourceBindings(sourceIds: readonly string[]): readonly HelpSourceBinding[] {
  const unique = [...new Set(sourceIds)].sort();
  if (unique.length === 0) {
    throw new Error("help authority: a cited context must have a source binding");
  }
  return Object.freeze(unique.map(getHelpSourceBinding));
}

/** Digest the source-byte records, never only `path#heading` locator names. */
export function helpSourceBindingDigest(sourceIds?: readonly string[]): string {
  const ids = sourceIds ?? HELP_CORPUS.sources.map((source) => source.id);
  return canonicalDigest(
    helpSourceBindings(ids).map((binding) => ({
      sourceId: binding.sourceId,
      sourceSectionDigest: binding.sourceSectionDigest,
      sourceByteLength: binding.sourceByteLength,
    })),
  );
}

export const HELP_AUTHORITY_IDENTITY: HelpAuthorityIdentity = Object.freeze({
  corpusDigest: HELP_CORPUS_DIGEST,
  sourceDigest: HELP_SOURCE_BYTE_DIGEST,
  modelDigest: HELP_MODEL_PROVENANCE.sha256,
  modelId: HELP_MODEL_PROVENANCE.modelId,
  modelVersion: HELP_MODEL_PROVENANCE.modelVersion,
});

if (HELP_AUTHORITY_IDENTITY.sourceDigest !== HELP_SOURCE_BYTE_DIGEST) {
  throw new Error("help authority: source-byte digest is not bound");
}
if (HELP_AUTHORITY_IDENTITY.corpusDigest !== HELP_CORPUS.digest) {
  throw new Error("help authority: corpus digest is not bound");
}
