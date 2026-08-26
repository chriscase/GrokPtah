/**
 * Public, Tauri-free integration surface.
 *
 * Keep this barrel limited to transport-neutral contracts and clients so it
 * can become the source for published `@grokptah/client` and
 * `@grokptah/ui-core` packages.
 */
export * from "./uiCore";
export * from "./grokptahBrokerClient";
export {
  HelpAuthorityBrokerClient,
  HelpAuthorityBrokerError,
  type HelpAuthorityBrokerOptions,
} from "./help/authority/broker";
