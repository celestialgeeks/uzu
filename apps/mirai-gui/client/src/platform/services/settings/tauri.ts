import { invoke } from "../shared/invoke";
import type { ModelParams } from "@/types/sampling";
import {
  DEFAULT_AUTO_EJECT_MINUTES,
  normalizeWebSearchSettings,
  type WebSearchTestResult,
  type SettingsService,
} from ".";

// Keys are read by every installed version; renaming one resets that
// preference on upgrade.

type StoredSettings = {
  analyticsEnabled?: boolean;
  modelChatNamingEnabled?: boolean;
  autoEjectEnabled?: boolean;
  autoEjectMinutes?: number;
  modelParams?: Record<string, ModelParams>;
  webSearchEnabled?: boolean;
  webSearchProvider?: string;
  webSearchApiKey?: string;
  webSearchSearxngUrl?: string;
  webSearchMaxResults?: number;
  webSearchContentMode?: string;
  webSearchContentMaxChars?: number;
  webSearchContentTruncate?: boolean;
};

const load = (): Promise<StoredSettings> => invoke<StoredSettings>("settings_load");

const patch = (values: Record<string, unknown>): Promise<void> => invoke("settings_patch", { patch: values });

const autoEjectEnabledOf = (s: StoredSettings): boolean => s.autoEjectEnabled !== false;
const autoEjectMinutesOf = (s: StoredSettings): number =>
  typeof s.autoEjectMinutes === "number" && Number.isFinite(s.autoEjectMinutes) && s.autoEjectMinutes > 0
    ? s.autoEjectMinutes
    : DEFAULT_AUTO_EJECT_MINUTES;

export const tauriSettings: SettingsService = {
  getAnalyticsEnabled: async () => (await load()).analyticsEnabled === true,
  setAnalyticsEnabled: (enabled) => patch({ analyticsEnabled: enabled }),
  getModelChatNamingEnabled: async () => (await load()).modelChatNamingEnabled !== false,
  setModelChatNamingEnabled: (enabled) => patch({ modelChatNamingEnabled: enabled }),
  getModelParams: async () => (await load()).modelParams ?? {},
  setModelParams: (repoId, params) => invoke("model_params_set", { repoId, params }),
  getAutoEjectEnabled: async () => autoEjectEnabledOf(await load()),
  // Rust writes the keys and applies them to its idle timer in one step.
  setAutoEjectEnabled: async (enabled) => {
    await invoke("set_auto_eject_config", { enabled });
    return enabled;
  },
  getAutoEjectMinutes: async () => autoEjectMinutesOf(await load()),
  setAutoEjectMinutes: async (minutes) => {
    await invoke("set_auto_eject_config", { minutes });
    return true;
  },
  getWebSearchSettings: async () => normalizeWebSearchSettings(await load()),
  // Same settings_patch merge as the other preferences: Rust stores the keys and
  // web_search_test reads them back, so saving first is all the wiring needed.
  setWebSearchSettings: (values) => patch({ ...values }),
  testWebSearch: async () => {
    try {
      return await invoke<WebSearchTestResult>("web_search_test");
    } catch (error) {
      // A rejected invoke carries the backend's message; surface it like the
      // structured failures so the tab has one rendering path.
      return {
        ok: false,
        error: {
          code: "request_failed",
          message: error instanceof Error ? error.message : String(error),
          userActionRequired: true,
        },
      };
    }
  },
};
