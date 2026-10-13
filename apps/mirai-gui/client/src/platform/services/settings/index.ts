import type { ModelParams } from "@/types/sampling";

export const DEFAULT_AUTO_EJECT_MINUTES = 15;

// Key names, provider ids and content modes are shared verbatim with the Rust
// side (`desktop/src/web`), which reads the same flat settings blob.

export const WEB_SEARCH_PROVIDERS = ["duckduckgo", "searxng", "brave", "tavily"] as const;
export type WebSearchProvider = (typeof WEB_SEARCH_PROVIDERS)[number];

export const WEB_SEARCH_CONTENT_MODES = ["snippet", "full"] as const;
export type WebSearchContentMode = (typeof WEB_SEARCH_CONTENT_MODES)[number];

export const WEB_SEARCH_MIN_RESULTS = 1;
export const WEB_SEARCH_MAX_RESULTS = 10;
export const WEB_SEARCH_DEFAULT_MAX_RESULTS = 3;
export const WEB_SEARCH_DEFAULT_CONTENT_MAX_CHARS = 20_000;
export const WEB_SEARCH_MAX_CONTENT_MAX_CHARS = 200_000;

export type WebSearchSettings = {
  webSearchEnabled: boolean;
  webSearchProvider: WebSearchProvider;
  webSearchApiKey: string;
  webSearchSearxngUrl: string;
  webSearchMaxResults: number;
  webSearchContentMode: WebSearchContentMode;
  webSearchContentMaxChars: number;
  webSearchContentTruncate: boolean;
};

// Off until the user opts in: the tool reaches the network, like analytics is
// explicit consent, and the provider defaults to keyless DuckDuckGo.
export const DEFAULT_WEB_SEARCH_SETTINGS: WebSearchSettings = {
  webSearchEnabled: false,
  webSearchProvider: "duckduckgo",
  webSearchApiKey: "",
  webSearchSearxngUrl: "",
  webSearchMaxResults: WEB_SEARCH_DEFAULT_MAX_RESULTS,
  webSearchContentMode: "snippet",
  webSearchContentMaxChars: WEB_SEARCH_DEFAULT_CONTENT_MAX_CHARS,
  webSearchContentTruncate: true,
};

// settings.json is an untyped merge, so numbers may arrive as strings or junk.
const storedNumber = (value: unknown): number | undefined => {
  if (typeof value === "number") return Number.isFinite(value) ? Math.round(value) : undefined;
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value.trim());
    return Number.isFinite(parsed) ? Math.round(parsed) : undefined;
  }
  return undefined;
};

// Mirrors Rust: max results clamps into the range, while a non-positive content
// limit falls back to the default instead of truncating everything away.
const clampedNumber = (value: unknown, fallback: number, min: number, max: number): number => {
  const parsed = storedNumber(value);
  return parsed === undefined ? fallback : Math.min(max, Math.max(min, parsed));
};

const positiveNumber = (value: unknown, fallback: number, max: number): number => {
  const parsed = storedNumber(value);
  return parsed === undefined || parsed <= 0 ? fallback : Math.min(max, parsed);
};

// Merge a partial stored blob with the defaults, clamping out-of-range numbers
// so a malformed settings.json still renders a usable tab. Settings arrive as an
// untyped JSON merge, so every field is read as unknown.
export const normalizeWebSearchSettings = (
  stored: Partial<Record<keyof WebSearchSettings, unknown>> | null | undefined,
): WebSearchSettings => {
  const fallback = DEFAULT_WEB_SEARCH_SETTINGS;
  const text = (value: unknown): string => (typeof value === "string" ? value : "");
  const provider = stored?.webSearchProvider;
  const contentMode = stored?.webSearchContentMode;
  return {
    webSearchEnabled: stored?.webSearchEnabled === true,
    webSearchProvider: WEB_SEARCH_PROVIDERS.includes(provider as WebSearchProvider)
      ? (provider as WebSearchProvider)
      : fallback.webSearchProvider,
    webSearchApiKey: text(stored?.webSearchApiKey),
    webSearchSearxngUrl: text(stored?.webSearchSearxngUrl),
    webSearchMaxResults: clampedNumber(
      stored?.webSearchMaxResults,
      fallback.webSearchMaxResults,
      WEB_SEARCH_MIN_RESULTS,
      WEB_SEARCH_MAX_RESULTS,
    ),
    webSearchContentMode: WEB_SEARCH_CONTENT_MODES.includes(contentMode as WebSearchContentMode)
      ? (contentMode as WebSearchContentMode)
      : fallback.webSearchContentMode,
    webSearchContentMaxChars: positiveNumber(
      stored?.webSearchContentMaxChars,
      fallback.webSearchContentMaxChars,
      WEB_SEARCH_MAX_CONTENT_MAX_CHARS,
    ),
    // Rust also defaults to truncating, so anything but an explicit false keeps it on.
    webSearchContentTruncate: stored?.webSearchContentTruncate !== false,
  };
};

export type WebSearchTestResult = {
  ok: boolean;
  provider?: string;
  results?: Array<{ title: string; url: string; snippet: string }>;
  error?: { code: string; message: string; userActionRequired: boolean };
};

export type SettingsService = {
  getAnalyticsEnabled(): Promise<boolean>;
  setAnalyticsEnabled(enabled: boolean): Promise<void>;
  getModelChatNamingEnabled(): Promise<boolean>;
  setModelChatNamingEnabled(enabled: boolean): Promise<void>;
  getModelParams(): Promise<Record<string, ModelParams>>;
  setModelParams(repoId: string, params: ModelParams | null): Promise<void>;
  getAutoEjectEnabled(): Promise<boolean>;
  setAutoEjectEnabled(enabled: boolean): Promise<boolean>;
  getAutoEjectMinutes(): Promise<number>;
  setAutoEjectMinutes(minutes: number): Promise<boolean>;
  getWebSearchSettings(): Promise<WebSearchSettings>;
  setWebSearchSettings(patch: Partial<WebSearchSettings>): Promise<void>;
  testWebSearch(): Promise<WebSearchTestResult>;
};
