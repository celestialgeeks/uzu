// Search providers and fetched pages supply these strings, so they are the one
// place a model-visible URL becomes an href. Anything that is not a plain http
// or https address (javascript:, data:, file:, relative text, empty) is refused
// rather than passed to an anchor.
const SAFE_PROTOCOLS = new Set(["http:", "https:"]);

const parse = (raw: unknown): URL | undefined => {
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  if (trimmed === "") return undefined;
  try {
    return new URL(trimmed);
  } catch {
    return undefined;
  }
};

/** The normalized URL when it is safe to link to, otherwise `undefined`. */
export const safeHttpUrl = (raw: unknown): string | undefined => {
  const url = parse(raw);
  return url && SAFE_PROTOCOLS.has(url.protocol) ? url.toString() : undefined;
};

/** Short readable host for a URL, used as the label on source cards. */
export const httpUrlHostname = (raw: unknown): string => {
  const hostname = parse(raw)?.hostname;
  return hostname ? hostname.replace(/^www\./, "") : "";
};
