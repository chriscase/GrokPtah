/**
 * Transport-neutral GrokPtah capability discovery.
 *
 * This module intentionally has no Tauri or React dependency. It can move to
 * a published client/UI package without changing consumer code.
 */

export const CAPABILITY_CONTRACT = "grokptah.capabilities.v1" as const;

export type CapabilityTier =
  | "observe"
  | "execute"
  | "review"
  | "promote"
  | "computer_observe"
  | "computer_control";

export type CapabilityAvailability = "available" | "gated" | "unavailable";

export type CapabilityDescriptor = {
  id: string;
  tier: CapabilityTier;
  mutating: boolean;
  human_gate: boolean;
  availability: CapabilityAvailability;
  description: string;
};

export type CapabilitySet = {
  contract: typeof CAPABILITY_CONTRACT;
  capabilities: CapabilityDescriptor[];
};

export type CapabilityActionState = "ready" | "requires_gate" | "unavailable";

const TIERS: ReadonlySet<CapabilityTier> = new Set([
  "observe",
  "execute",
  "review",
  "promote",
  "computer_observe",
  "computer_control",
]);

const AVAILABILITIES: ReadonlySet<CapabilityAvailability> = new Set([
  "available",
  "gated",
  "unavailable",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const CAPABILITY_ID = /^[a-z][a-z0-9]*(\.[a-z][a-z0-9_]*)+$/;
const DESCRIPTOR_KEYS = new Set([
  "id",
  "tier",
  "mutating",
  "human_gate",
  "availability",
  "description",
]);
const CAPABILITY_SET_KEYS = new Set(["contract", "capabilities"]);
const MAX_DESCRIPTION_LENGTH = 512;

function parseDescriptor(value: unknown): CapabilityDescriptor | null {
  if (!isRecord(value)) return null;
  if (Object.keys(value).some((key) => !DESCRIPTOR_KEYS.has(key))) return null;
  const { id, tier, mutating, human_gate, availability, description } = value;
  if (
    typeof id !== "string" ||
    !CAPABILITY_ID.test(id) ||
    typeof tier !== "string" ||
    !TIERS.has(tier as CapabilityTier) ||
    typeof mutating !== "boolean" ||
    typeof human_gate !== "boolean" ||
    typeof availability !== "string" ||
    !AVAILABILITIES.has(availability as CapabilityAvailability) ||
    typeof description !== "string" ||
    description.length === 0 ||
    description.length > MAX_DESCRIPTION_LENGTH ||
    (availability === "gated" && human_gate !== true)
  ) {
    return null;
  }
  return {
    id,
    tier: tier as CapabilityTier,
    mutating,
    human_gate,
    availability: availability as CapabilityAvailability,
    description,
  };
}

/** Parse an initialize response without trusting an unknown contract version. */
export function parseCapabilitySet(value: unknown): CapabilitySet | null {
  if (!isRecord(value) || value.contract !== CAPABILITY_CONTRACT) return null;
  if (Object.keys(value).some((key) => !CAPABILITY_SET_KEYS.has(key))) return null;
  if (!Array.isArray(value.capabilities)) return null;
  const capabilities = value.capabilities.map(parseDescriptor);
  if (capabilities.some((capability) => capability === null)) return null;
  const ids = capabilities.map((capability) => capability!.id);
  if (new Set(ids).size !== ids.length) return null;
  return {
    contract: CAPABILITY_CONTRACT,
    capabilities: capabilities as CapabilityDescriptor[],
  };
}

/** Find a capability by stable id. */
export function findCapability(
  set: CapabilitySet | null | undefined,
  id: string,
): CapabilityDescriptor | undefined {
  return set?.capabilities.find((capability) => capability.id === id);
}

/**
 * Return the UI action state. A gated capability is renderable but cannot be
 * invoked until the caller has obtained the required human/lease approval.
 */
export function capabilityActionState(
  capability: CapabilityDescriptor | undefined,
  gateSatisfied = false,
): CapabilityActionState {
  if (!capability || capability.availability === "unavailable") {
    return "unavailable";
  }
  if (capability.availability === "gated" || capability.human_gate) {
    return gateSatisfied ? "ready" : "requires_gate";
  }
  return "ready";
}
