/**
 * The corpus this bundle ships: the public one, and only the public one.
 *
 * TypeScript deliberately does not import `help-corpus.v1.json`. Rust embeds
 * that file with `include_str!` and the host serves from it; if any module
 * under `src/lib` imported it, every bundle transitively importing that module
 * would carry the restricted text — which is precisely what happened when
 * `publicSurface` imported its verifier from a module that loaded the full
 * corpus. `helpBundle.test.ts` fails the build if the private artifact is
 * reachable from the published entry points again.
 *
 * A principal entitled to more than the public set does not get it from the
 * bundle. It asks the host, which filters and returns the corpus that
 * principal may see (`helpVisibleCorpus`). Content above a reader's ceiling
 * never crosses the boundary, so there is nothing in the renderer for a
 * modified renderer to reveal.
 *
 * Verification is synchronous at module load and fails closed: if a stored
 * digest disagrees with the bytes it names, importing this module throws.
 * Serving a degraded corpus would mean answering from content nobody reviewed.
 */

import publicCorpusJson from "./help-corpus-public.v1.json";
import type { HelpArticle, HelpChunk, HelpCorpus, HelpSourceAnchor } from "../generated/contract";
import { parseHelpCorpus } from "./schema";
import { chunksForArticle, findArticle, findChunk, findSource, verifyHelpCorpus } from "./verify";

export {
  HelpCorpusDigestMismatchError,
  isPublicOnly,
  verifyHelpCorpus,
  chunksForArticle as chunksForArticleIn,
  findArticle as findArticleIn,
  findChunk as findChunkIn,
  findSource as findSourceIn,
} from "./verify";
export { HelpCorpusSchemaError, parseHelpCorpus } from "./schema";

/** The public Help corpus, verified at load. */
export const HELP_PUBLIC_CORPUS: HelpCorpus = parseHelpCorpus(publicCorpusJson);

verifyHelpCorpus(HELP_PUBLIC_CORPUS);

export const HELP_PUBLIC_CORPUS_DIGEST = HELP_PUBLIC_CORPUS.digest;
export const HELP_PUBLIC_CORPUS_CONTENT_VERSION = HELP_PUBLIC_CORPUS.content_version;

/**
 * The corpus a caller gets when it does not supply one.
 *
 * An alias for the public corpus. Named separately so the reason is visible at
 * every use site: this is the floor every reader is entitled to, not "the
 * corpus".
 */
export const HELP_CORPUS = HELP_PUBLIC_CORPUS;

export function getHelpArticle(id: string): HelpArticle | undefined {
  return findArticle(HELP_PUBLIC_CORPUS, id);
}

export function getHelpChunk(id: string): HelpChunk | undefined {
  return findChunk(HELP_PUBLIC_CORPUS, id);
}

export function getHelpSource(id: string): HelpSourceAnchor | undefined {
  return findSource(HELP_PUBLIC_CORPUS, id);
}

/** Chunks belonging to one article of the public corpus, in corpus order. */
export function chunksForPublicArticle(articleId: string): readonly HelpChunk[] {
  return chunksForArticle(HELP_PUBLIC_CORPUS, articleId);
}
