/**
 * `@grokptah/client/help-react` entry: the published Help surface plus its
 * React primitives. Separate from `ui-core` so that bundle keeps no React
 * dependency.
 *
 * The surface is `./help/publicSurface`, not the internal barrel. See that
 * module for what is withheld and why.
 */
export * from "./help/publicSurface";
export * from "./help/react/index";
