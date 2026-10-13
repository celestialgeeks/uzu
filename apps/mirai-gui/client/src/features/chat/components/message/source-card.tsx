import { Globe, Search } from "lucide-react";
import type { ReactNode } from "react";
import type { TranscriptSearchResult } from "@/types/llm-stream";
import { httpUrlHostname, safeHttpUrl } from "@/utils/external-url";

const CARD_CLASSES =
  "my-2 min-w-0 rounded-md border border-border-default bg-surface-elevated px-3 py-2 text-[13px] leading-[150%]";

const CardRow = ({
  icon,
  label,
  failed,
  children,
}: {
  icon: ReactNode;
  label: string;
  failed: boolean;
  children?: ReactNode;
}) => (
  <div className="flex items-center gap-2">
    <span aria-hidden="true" className="shrink-0 text-label-muted">
      {icon}
    </span>
    <span className={failed ? "min-w-0 truncate text-error" : "min-w-0 truncate text-label-muted"}>{label}</span>
    {children}
  </div>
);

const resultLabel = (title: string, href?: string): string => {
  const trimmed = title.trim();
  if (trimmed !== "") return trimmed;
  const host = httpUrlHostname(href);
  return host !== "" ? host : "Untitled result";
};

/** The `search` transcript item: the query the model ran and the hits it got back. */
export function SearchSourceCard({
  query,
  results,
  failed,
}: {
  query: string;
  results: TranscriptSearchResult[];
  failed: boolean;
}) {
  // Guarded like the chart card: a malformed snapshot must not throw mid-stream.
  const hits = Array.isArray(results) ? results : [];
  return (
    <div data-search-card className={CARD_CLASSES}>
      <CardRow
        icon={<Search className="size-3.5" />}
        failed={failed}
        label={failed ? `Couldn't search for “${query}”` : `Searched the web for “${query}”`}
      />
      {failed ? null : hits.length === 0 ? (
        <p className="mt-1 pl-5 text-label-muted">No results.</p>
      ) : (
        <ul className="mt-1 flex flex-col gap-0.5">
          {hits.map((result, index) => {
            const href = safeHttpUrl(result.url);
            return (
              <li key={index} className="min-w-0 truncate pl-5" title={href}>
                {href ? (
                  <a href={href} target="_blank" rel="noopener noreferrer" className="text-label-title hover:underline">
                    {resultLabel(result.title, href)}
                  </a>
                ) : (
                  // Only http(s) becomes a link; anything else a provider returns stays text.
                  <span className="text-label-muted">{resultLabel(result.title)}</span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/** The `fetch` transcript item: one page the model read. */
export function FetchSourceCard({ url, truncated, failed }: { url: string; truncated: boolean; failed: boolean }) {
  const href = failed ? undefined : safeHttpUrl(url);
  const host = httpUrlHostname(url);
  const label = host !== "" ? host : "a page";
  return (
    <div data-fetch-card className={CARD_CLASSES}>
      <CardRow
        icon={<Globe className="size-3.5" />}
        failed={failed}
        label={failed ? `Couldn't read ${label}` : `Read ${label}`}
      >
        {!failed && truncated && <span className="shrink-0 text-label-muted">· truncated</span>}
        {href && (
          <a
            href={href}
            target="_blank"
            rel="noopener noreferrer"
            title={href}
            className="shrink-0 text-label-title hover:underline"
          >
            Open
          </a>
        )}
      </CardRow>
    </div>
  );
}
