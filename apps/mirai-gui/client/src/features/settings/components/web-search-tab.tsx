import { useEffect, useState } from "react";
import { CircleAlert, CircleCheck, Globe } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Toggle } from "@/components/ui/toggle";
import { getPlatform } from "@/platform/platform-singleton";
import {
  DEFAULT_WEB_SEARCH_SETTINGS,
  WEB_SEARCH_CONTENT_MODES,
  WEB_SEARCH_MAX_CONTENT_MAX_CHARS,
  WEB_SEARCH_MAX_RESULTS,
  WEB_SEARCH_MIN_RESULTS,
  type WebSearchContentMode,
  type WebSearchProvider,
  type WebSearchSettings,
  type WebSearchTestResult,
} from "@/platform/services/settings";
import { SettingDivider, SettingRow } from "./general/setting-row";

// Providers that cannot work without a credential.
const KEYED_PROVIDERS: WebSearchProvider[] = ["brave", "tavily"];

const PROVIDER_OPTIONS = [
  { value: "duckduckgo", label: "DuckDuckGo" },
  { value: "searxng", label: "SearXNG" },
  { value: "brave", label: "Brave" },
  { value: "tavily", label: "Tavily" },
] as const satisfies ReadonlyArray<{ value: WebSearchProvider; label: string }>;

const CONTENT_MODE_LABELS: Record<WebSearchContentMode, string> = {
  snippet: "Snippets",
  full: "Full pages",
};

type NumberFieldProps = {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (value: number) => void;
};

/** Commits on blur like the auto-eject interval, so typing "1" is not read as one result. */
function NumberField({ label, value, min, max, onChange }: NumberFieldProps) {
  const [draft, setDraft] = useState(String(value));

  useEffect(() => {
    setDraft((current) => (Number(current) === value ? current : String(value)));
  }, [value]);

  const commit = () => {
    const parsed = Math.floor(Number(draft));
    const next = Number.isFinite(parsed) ? Math.min(max, Math.max(min, parsed)) : value;
    setDraft(String(next));
    if (next !== value) onChange(next);
  };

  return (
    <Input
      size="sm"
      type="text"
      inputMode="numeric"
      aria-label={label}
      className="max-w-[120px]"
      value={draft}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={commit}
    />
  );
}

export default function WebSearchTab() {
  const [settings, setSettings] = useState<WebSearchSettings>(DEFAULT_WEB_SEARCH_SETTINGS);
  const [testing, setTesting] = useState(false);
  const [outcome, setOutcome] = useState<WebSearchTestResult | null>(null);

  useEffect(() => {
    let active = true;
    void getPlatform()
      .settings.getWebSearchSettings()
      .then((loaded) => {
        if (active) setSettings(loaded);
      })
      .catch((error: unknown) => console.warn("getWebSearchSettings failed", error));
    return () => {
      active = false;
    };
  }, []);

  // Every edit invalidates the last run: the result describes the settings that
  // were saved when it was produced.
  const save = async (patch: Partial<WebSearchSettings>) => {
    const previous = settings;
    setSettings({ ...previous, ...patch });
    setOutcome(null);
    try {
      await getPlatform().settings.setWebSearchSettings(patch);
    } catch (error) {
      setSettings(previous);
      console.error("Failed to save web search settings", error);
    }
  };

  const test = async () => {
    if (testing) return;
    setTesting(true);
    setOutcome(null);
    try {
      // The backend reads the just-saved keys, so the test reflects what was written.
      await getPlatform().settings.setWebSearchSettings(settings);
      setOutcome(await getPlatform().settings.testWebSearch());
    } catch (error) {
      console.error("Web search test failed", error);
      setOutcome({
        ok: false,
        error: { code: "request_failed", message: String(error), userActionRequired: true },
      });
    } finally {
      setTesting(false);
    }
  };

  const needsKey = KEYED_PROVIDERS.includes(settings.webSearchProvider);
  const results = outcome?.results?.length ?? 0;

  return (
    <div className="h-full flex flex-col">
      <div className="flex flex-col gap-5 pb-5">
        <SettingRow
          title="Web search"
          description="Let models search the web and read the results while answering."
          control={
            <Toggle
              label="Web search"
              checked={settings.webSearchEnabled}
              onChange={(enabled) => void save({ webSearchEnabled: enabled })}
            />
          }
        />

        <SettingDivider />
        <SettingRow
          title="Search provider"
          description="DuckDuckGo needs no account. SearXNG, Brave, and Tavily are configured below."
          control={
            <SegmentedControl
              ariaLabel="Search provider"
              value={settings.webSearchProvider}
              onChange={(value) => void save({ webSearchProvider: value as WebSearchProvider })}
              options={PROVIDER_OPTIONS.map((option) => ({ ...option, ariaLabel: option.label }))}
            />
          }
        />

        {needsKey && (
          <>
            <SettingDivider />
            <SettingRow
              title="API key"
              description={`Stored on this computer and sent only to ${settings.webSearchProvider}.`}
              control={
                <Input
                  size="sm"
                  type="password"
                  aria-label="Search provider API key"
                  autoComplete="off"
                  spellCheck={false}
                  className="w-[260px]"
                  value={settings.webSearchApiKey}
                  onChange={(event) => void save({ webSearchApiKey: event.target.value })}
                />
              }
            />
          </>
        )}

        {settings.webSearchProvider === "searxng" && (
          <>
            <SettingDivider />
            <SettingRow
              title="SearXNG instance URL"
              description="The base URL of your instance, including https://."
              control={
                <Input
                  size="sm"
                  type="url"
                  aria-label="SearXNG instance URL"
                  placeholder="https://searx.example.org"
                  autoComplete="off"
                  spellCheck={false}
                  className="w-[260px]"
                  value={settings.webSearchSearxngUrl}
                  onChange={(event) => void save({ webSearchSearxngUrl: event.target.value })}
                />
              }
            />
          </>
        )}

        <SettingDivider />
        <SettingRow
          title="Maximum results"
          description={`How many links each search returns, from ${WEB_SEARCH_MIN_RESULTS} to ${WEB_SEARCH_MAX_RESULTS}.`}
          control={
            <NumberField
              label="Maximum results"
              value={settings.webSearchMaxResults}
              min={WEB_SEARCH_MIN_RESULTS}
              max={WEB_SEARCH_MAX_RESULTS}
              onChange={(webSearchMaxResults) => void save({ webSearchMaxResults })}
            />
          }
        />

        <SettingDivider />
        <SettingRow
          title="Page content"
          description={
            settings.webSearchContentMode === "full"
              ? "Full pages: Mirai fetches every result page, so one search makes one request per result."
              : "Snippets only: the short summary the provider already returns."
          }
          control={
            <SegmentedControl
              ariaLabel="Page content"
              value={settings.webSearchContentMode}
              onChange={(value) => void save({ webSearchContentMode: value as WebSearchContentMode })}
              options={WEB_SEARCH_CONTENT_MODES.map((mode) => ({ value: mode, label: CONTENT_MODE_LABELS[mode] }))}
            />
          }
        />

        <SettingDivider />
        <SettingRow
          title="Content character limit"
          description={`Text taken from each page is capped at this many characters, up to ${WEB_SEARCH_MAX_CONTENT_MAX_CHARS}.`}
          control={
            <NumberField
              label="Content character limit"
              value={settings.webSearchContentMaxChars}
              min={1}
              max={WEB_SEARCH_MAX_CONTENT_MAX_CHARS}
              onChange={(webSearchContentMaxChars) => void save({ webSearchContentMaxChars })}
            />
          }
        />

        <SettingDivider />
        <SettingRow
          title="Truncate long pages"
          description="Cut fetched pages to the character limit above. Turn this off to pass longer pages through in full."
          control={
            <Toggle
              label="Truncate long pages"
              checked={settings.webSearchContentTruncate}
              onChange={(webSearchContentTruncate) => void save({ webSearchContentTruncate })}
            />
          }
        />

        <SettingDivider />
        <div className="px-5 lg:max-w-[800px] mx-auto w-full">
          <div className="flex flex-col gap-2">
            <Button
              kind="secondary"
              size="xs"
              icon={<Globe className="size-3.5" />}
              ariaLabel="Test search"
              loading={testing}
              disabled={testing}
              onClick={() => void test()}
            >
              {testing ? "Testing…" : "Test search"}
            </Button>

            {outcome && (
              <p
                role="status"
                className="flex items-start gap-2 text-[13px] leading-[150%]"
                data-outcome={outcome.ok ? "success" : "failure"}
              >
                {outcome.ok ? (
                  <>
                    <CircleCheck aria-hidden="true" className="mt-0.5 size-3.5 shrink-0 text-success" />
                    <span className="text-success">
                      {outcome.provider ?? settings.webSearchProvider} returned {results}{" "}
                      {results === 1 ? "result" : "results"}.
                    </span>
                  </>
                ) : (
                  <>
                    <CircleAlert aria-hidden="true" className="mt-0.5 size-3.5 shrink-0 text-error" />
                    <span className="text-error">
                      {outcome.error?.message ?? "The search could not be completed."} Check the provider and its key or
                      instance URL above.
                    </span>
                  </>
                )}
              </p>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
