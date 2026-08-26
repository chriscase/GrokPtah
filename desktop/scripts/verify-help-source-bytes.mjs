/**
 * Verify the source-byte identity manifest used by the Help authority contract.
 *
 * A path and heading are locator metadata, not proof of the content that was
 * cited. This verifier canonicalizes each cited Markdown section, hashes its
 * exact UTF-8 bytes, and compares the result with the checked-in runtime
 * manifest. `--write` is only for a reviewed source change.
 */
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";

import { HELP_SOURCE_REGISTRY } from "../src/lib/help/canonical/data.ts";
import { canonicalJson } from "../src/lib/help/canonical/digest.ts";

const here = new URL(".", import.meta.url);
const repoRoot = resolve(new URL("../../", here).pathname);
const manifestPath = new URL("../src/lib/help/canonical/sourceManifest.ts", import.meta.url);

function sha256(value) {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

function sectionFor(text, heading) {
  const normalized = text.replace(/\r\n?/gu, "\n");
  const headingPattern = /^(#{1,6})[ \t]+(.+?)\s*$/gmu;
  const matches = [];
  for (const match of normalized.matchAll(headingPattern)) {
    if (match[2].trim() === heading) matches.push(match);
  }
  if (matches.length !== 1) {
    throw new Error(`source heading must resolve exactly once: ${heading} (${matches.length})`);
  }
  const startMatch = matches[0];
  const start = startMatch.index;
  const level = startMatch[1].length;
  let end = normalized.length;
  headingPattern.lastIndex = startMatch.index + startMatch[0].length;
  for (const match of normalized.matchAll(headingPattern)) {
    if (match.index > start && match[1].length <= level) {
      end = match.index;
      break;
    }
  }
  const section = normalized.slice(start, end);
  const prefixBytes = Buffer.byteLength(normalized.slice(0, start), "utf8");
  return {
    headingLevel: level,
    startByte: prefixBytes,
    endByte: prefixBytes + Buffer.byteLength(section, "utf8"),
    byteLength: Buffer.byteLength(section, "utf8"),
    digest: sha256(Buffer.from(section, "utf8")),
  };
}

async function buildManifest() {
  const sources = {};
  for (const sourceId of Object.keys(HELP_SOURCE_REGISTRY).sort()) {
    const source = HELP_SOURCE_REGISTRY[sourceId];
    if (
      source.path.startsWith("/") ||
      source.path.includes("\\") ||
      source.path.split("/").includes("..")
    ) {
      throw new Error(`source ${sourceId} is not repository-relative`);
    }
    const path = join(repoRoot, source.path);
    if (!existsSync(path)) throw new Error(`source ${sourceId} is missing ${source.path}`);
    const section = sectionFor(await readFile(path, "utf8"), source.heading);
    sources[sourceId] = {
      sourceId,
      path: source.path,
      heading: source.heading,
      ...section,
    };
  }
  const sourceDigest = sha256(
    canonicalJson(
      Object.values(sources).map(({ sourceId, byteLength, digest }) => ({
        sourceId,
        sourceSectionDigest: digest,
        sourceByteLength: byteLength,
      })),
    ),
  );
  return { sources, sourceDigest };
}

function render(manifest) {
  return `/**
 * GENERATED FILE - DO NOT EDIT.
 * Rebuild with: npm run help:verify-source-bytes -- --write
 */
export type HelpSourceByteIdentity = {
  readonly sourceId: string;
  readonly path: string;
  readonly heading: string;
  readonly headingLevel: number;
  readonly startByte: number;
  readonly endByte: number;
  readonly byteLength: number;
  readonly digest: string;
};

export const HELP_SOURCE_BYTE_IDENTITIES = Object.freeze(${JSON.stringify(manifest.sources, null, 2)}) as Readonly<
  Record<string, HelpSourceByteIdentity>
>;

export const HELP_SOURCE_BYTE_DIGEST = ${JSON.stringify(manifest.sourceDigest)} as const;
`;
}

const manifest = await buildManifest();
if (process.argv.includes("--write")) {
  await writeFile(manifestPath, render(manifest));
  console.log(`wrote Help source-byte manifest: ${manifest.sourceDigest}`);
} else {
  if (!existsSync(manifestPath)) {
    throw new Error("sourceManifest.ts is missing; run with --write after reviewing source anchors");
  }
  const { HELP_SOURCE_BYTE_IDENTITIES, HELP_SOURCE_BYTE_DIGEST } = await import(
    manifestPath.href
  );
  if (JSON.stringify(HELP_SOURCE_BYTE_IDENTITIES) !== JSON.stringify(manifest.sources)) {
    throw new Error("Help source-byte identity manifest drifted from repository bytes");
  }
  if (HELP_SOURCE_BYTE_DIGEST !== manifest.sourceDigest) {
    throw new Error(
      `Help source-byte digest drifted: locked ${HELP_SOURCE_BYTE_DIGEST} != actual ${manifest.sourceDigest}`,
    );
  }
  console.log(`Help source bytes verified: ${Object.keys(manifest.sources).length} sections`);
  console.log(`  sourceByteDigest: ${manifest.sourceDigest}`);
}
