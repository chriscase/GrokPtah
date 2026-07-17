/** Local appearance chrome beyond theme (#134). */

export type AccentId = "amber" | "teal" | "violet";
export type DensityId = "compact" | "comfortable" | "spacious";
export type TypeScaleId = "sm" | "md" | "lg";

export type AppearanceChrome = {
  accent: AccentId;
  density: DensityId;
  typeScale: TypeScaleId;
};

const KEY = "grokptah.appearance.chrome";

const DEFAULTS: AppearanceChrome = {
  accent: "amber",
  density: "comfortable",
  typeScale: "md",
};

export function loadAppearanceChrome(): AppearanceChrome {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<AppearanceChrome>;
    return {
      accent: (parsed.accent as AccentId) || DEFAULTS.accent,
      density: (parsed.density as DensityId) || DEFAULTS.density,
      typeScale: (parsed.typeScale as TypeScaleId) || DEFAULTS.typeScale,
    };
  } catch {
    return { ...DEFAULTS };
  }
}

export function saveAppearanceChrome(c: AppearanceChrome): void {
  localStorage.setItem(KEY, JSON.stringify(c));
  applyAppearanceChrome(c);
}

export function applyAppearanceChrome(c: AppearanceChrome): void {
  const root = document.documentElement;
  root.dataset.accent = c.accent;
  root.dataset.density = c.density;
  root.dataset.typeScale = c.typeScale;
}
