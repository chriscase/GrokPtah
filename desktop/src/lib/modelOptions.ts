import type { ModelInfo } from "./protocol";

export const LEGACY_EFFORT_OPTIONS = ["low", "medium", "high", "xhigh"] as const;

/** Return only effort values the selected model advertises or can accept. */
export function effortOptionsForModel(
  models: ModelInfo[],
  modelId?: string | null,
): string[] {
  const model = models.find((entry) => entry.id === modelId);
  if (!model) return ["medium"];
  if (!model.supports_effort) return ["none"];
  if (model.effort_options && model.effort_options.length > 0) {
    return model.effort_options;
  }
  return [...LEGACY_EFFORT_OPTIONS];
}

export function effortForModel(
  models: ModelInfo[],
  modelId: string | null | undefined,
  current: string | null | undefined,
): string {
  const options = effortOptionsForModel(models, modelId);
  return current && options.includes(current) ? current : (options[0] ?? "none");
}
