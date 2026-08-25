/**
 * React primitives for embedding Help retrieval.
 *
 * Kept out of the headless barrel so `@grokptah/client` and its `ui-core`
 * subpath stay dependency-free; React is an external of this entry only.
 */
export {
  HelpCitationList,
  HelpHighlightedText,
  HelpResultItem,
  HelpResults,
  HelpSearchInput,
  useHelpSearch,
  type HelpCitationListProps,
  type HelpHighlightedTextProps,
  type HelpResultItemProps,
  type HelpResultsProps,
  type HelpSearchInputProps,
} from "./primitives";
export {
  HelpRoute,
  HelpRouteFooter,
  useHelpPaletteShortcut,
  type HelpProviderState,
  type HelpRouteProps,
} from "./HelpRoute";
