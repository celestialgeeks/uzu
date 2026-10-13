import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DEFAULT_WEB_SEARCH_SETTINGS } from "@/platform/services/settings";
import WebSearchTab from "./web-search-tab";

vi.mock("@/platform/platform-info", () => ({
  platformInfo: { isTauri: true, features: { webSearch: true } },
}));

const settings = {
  getWebSearchSettings: vi.fn(),
  setWebSearchSettings: vi.fn(async () => {}),
  testWebSearch: vi.fn(),
};

vi.mock("@/platform/platform-singleton", () => ({ getPlatform: () => ({ settings }) }));

beforeEach(() => {
  vi.clearAllMocks();
  settings.getWebSearchSettings.mockResolvedValue({ ...DEFAULT_WEB_SEARCH_SETTINGS });
  settings.setWebSearchSettings.mockResolvedValue(undefined);
});

afterEach(() => cleanup());

it("shows only the fields the selected provider needs", () => {
  render(<WebSearchTab />);
  expect(screen.getByRole("radio", { name: "DuckDuckGo" }).getAttribute("aria-checked")).toBe("true");
  expect(screen.queryByLabelText("Search provider API key")).toBeNull();
  expect(screen.queryByRole("textbox", { name: "SearXNG instance URL" })).toBeNull();

  fireEvent.click(screen.getByRole("radio", { name: "Brave" }));
  expect(screen.getByLabelText("Search provider API key")).toBeTruthy();
  expect(screen.queryByRole("textbox", { name: "SearXNG instance URL" })).toBeNull();

  fireEvent.click(screen.getByRole("radio", { name: "Tavily" }));
  expect(screen.getByLabelText("Search provider API key")).toBeTruthy();

  fireEvent.click(screen.getByRole("radio", { name: "SearXNG" }));
  expect(screen.getByRole("textbox", { name: "SearXNG instance URL" })).toBeTruthy();
  expect(screen.queryByLabelText("Search provider API key")).toBeNull();

  expect(settings.setWebSearchSettings).toHaveBeenLastCalledWith({ webSearchProvider: "searxng" });
});

it("writes each control through the settings service with the exact keys", () => {
  render(<WebSearchTab />);

  fireEvent.click(screen.getByRole("switch", { name: "Web search" }));
  expect(settings.setWebSearchSettings).toHaveBeenLastCalledWith({ webSearchEnabled: true });

  fireEvent.click(screen.getByRole("switch", { name: "Truncate long pages" }));
  expect(settings.setWebSearchSettings).toHaveBeenLastCalledWith({ webSearchContentTruncate: false });

  fireEvent.click(screen.getByRole("radio", { name: "Full pages" }));
  expect(settings.setWebSearchSettings).toHaveBeenLastCalledWith({ webSearchContentMode: "full" });

  const maxResults = screen.getByRole("textbox", { name: "Maximum results" }) as HTMLInputElement;
  expect(maxResults.value).toBe("3");
  fireEvent.change(maxResults, { target: { value: "99" } });
  fireEvent.blur(maxResults);
  expect(settings.setWebSearchSettings).toHaveBeenLastCalledWith({ webSearchMaxResults: 10 });
  expect(maxResults.value).toBe("10");

  const limit = screen.getByRole("textbox", { name: "Content character limit" }) as HTMLInputElement;
  expect(limit.value).toBe("20000");
  fireEvent.change(limit, { target: { value: "5000" } });
  fireEvent.blur(limit);
  expect(settings.setWebSearchSettings).toHaveBeenLastCalledWith({ webSearchContentMaxChars: 5000 });
});

it("saves before testing and reports the provider and result count", async () => {
  settings.testWebSearch.mockResolvedValue({
    ok: true,
    provider: "duckduckgo",
    results: [
      { title: "One", url: "https://one.test", snippet: "a" },
      { title: "Two", url: "https://two.test", snippet: "b" },
    ],
  });
  render(<WebSearchTab />);

  fireEvent.click(screen.getByRole("button", { name: "Test search" }));
  const button = screen.getByRole("button", { name: "Test search" }) as HTMLButtonElement;
  expect(button.disabled).toBe(true);
  // The pending save completes before the backend command runs.
  await waitFor(() => expect(settings.testWebSearch).toHaveBeenCalledOnce());
  expect(settings.setWebSearchSettings).toHaveBeenCalledWith(
    expect.objectContaining({ webSearchProvider: "duckduckgo" }),
  );

  const outcome = await screen.findByRole("status");
  expect(outcome.getAttribute("data-outcome")).toBe("success");
  expect(outcome.textContent).toContain("duckduckgo returned 2 results");
  await waitFor(() => expect(button.disabled).toBe(false));
});

it("shows one result in the singular", async () => {
  settings.testWebSearch.mockResolvedValue({
    ok: true,
    provider: "brave",
    results: [{ title: "One", url: "https://x.test", snippet: "" }],
  });
  render(<WebSearchTab />);
  fireEvent.click(screen.getByRole("button", { name: "Test search" }));
  expect((await screen.findByRole("status")).textContent).toContain("brave returned 1 result");
});

it("shows the backend message verbatim with a hint about the provider", async () => {
  settings.testWebSearch.mockResolvedValue({
    ok: false,
    error: {
      code: "invalid_authentication",
      message: "Brave Search rejected the configured credential (HTTP 401). Check the key in Settings, Web Search.",
      userActionRequired: true,
    },
  });
  render(<WebSearchTab />);
  fireEvent.click(screen.getByRole("button", { name: "Test search" }));

  const outcome = await screen.findByRole("status");
  expect(outcome.getAttribute("data-outcome")).toBe("failure");
  expect(outcome.textContent).toContain(
    "Brave Search rejected the configured credential (HTTP 401). Check the key in Settings, Web Search.",
  );
  expect(outcome.textContent).toContain("Check the provider");
});

it("clears the previous outcome when a setting changes again", async () => {
  settings.testWebSearch.mockResolvedValue({ ok: true, provider: "tavily", results: [] });
  render(<WebSearchTab />);
  fireEvent.click(screen.getByRole("button", { name: "Test search" }));
  await screen.findByRole("status");

  fireEvent.click(screen.getByRole("radio", { name: "Brave" }));
  expect(screen.queryByRole("status")).toBeNull();
});

it("restores the previous values when the settings write fails", async () => {
  settings.setWebSearchSettings.mockRejectedValue(new Error("disk full"));
  render(<WebSearchTab />);

  expect(screen.getByRole("switch", { name: "Web search" }).getAttribute("aria-checked")).toBe("false");
  fireEvent.click(screen.getByRole("switch", { name: "Web search" }));
  expect(screen.getByRole("switch", { name: "Web search" }).getAttribute("aria-checked")).toBe("true");

  await waitFor(() =>
    expect(screen.getByRole("switch", { name: "Web search" }).getAttribute("aria-checked")).toBe("false"),
  );
});

it("loads the saved settings instead of the defaults", async () => {
  settings.getWebSearchSettings.mockResolvedValue({
    ...DEFAULT_WEB_SEARCH_SETTINGS,
    webSearchEnabled: true,
    webSearchProvider: "tavily",
    webSearchApiKey: "tvly-secret",
    webSearchMaxResults: 8,
    webSearchContentMode: "full",
    webSearchContentTruncate: false,
  });
  render(<WebSearchTab />);

  await waitFor(() =>
    expect(screen.getByRole("switch", { name: "Web search" }).getAttribute("aria-checked")).toBe("true"),
  );
  expect(screen.getByRole("radio", { name: "Tavily" }).getAttribute("aria-checked")).toBe("true");
  expect((screen.getByLabelText("Search provider API key") as HTMLInputElement).value).toBe("tvly-secret");
  expect((screen.getByRole("textbox", { name: "Maximum results" }) as HTMLInputElement).value).toBe("8");
  expect(screen.getByRole("radio", { name: "Full pages" }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByRole("switch", { name: "Truncate long pages" }).getAttribute("aria-checked")).toBe("false");
});
