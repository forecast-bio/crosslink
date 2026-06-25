// /settings/webhooks — outbound alert-delivery configuration
// (design doc §14 Phase 5 — webhook alerting).
//
// The dashboard fires a POST at each configured URL whenever an alert
// transitions from "not derived" → "derived". Users paste Slack /
// Discord / generic-JSON endpoints here. The backend routes the
// payload shape based on the URL host — see
// crosslink/src/dashboard/webhook.rs.

import { useEffect, useState } from "react";

import { useWebhooks, useSetWebhooks } from "@/api/client";

/// Trim-aware dedupe that preserves first-occurrence order.
function normalize(urls: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of urls) {
    const t = raw.trim();
    if (!t || seen.has(t)) continue;
    seen.add(t);
    out.push(t);
  }
  return out;
}

export function SettingsWebhooks() {
  const loaded = useWebhooks();
  const save = useSetWebhooks();

  // Local draft list the user edits; synced from server data once on
  // first load and whenever a fresh refetch arrives while not saving.
  const [draft, setDraft] = useState<string[]>([]);
  const [newUrl, setNewUrl] = useState("");

  useEffect(() => {
    if (loaded.data && !save.isPending) {
      setDraft(loaded.data.urls);
    }
  }, [loaded.data, save.isPending]);

  function addUrl() {
    const t = newUrl.trim();
    if (!t) return;
    setDraft((prev) => normalize([...prev, t]));
    setNewUrl("");
  }

  function removeAt(idx: number) {
    setDraft((prev) => prev.filter((_, i) => i !== idx));
  }

  function onSave() {
    save.mutate({ urls: normalize(draft) });
  }

  const serverUrls = loaded.data?.urls ?? [];
  const dirty =
    serverUrls.length !== draft.length ||
    serverUrls.some((u, i) => u !== draft[i]);

  return (
    <main className="mx-auto max-w-4xl px-6 py-6">
      <header className="mb-4">
        <h1 className="text-2xl font-semibold">Webhook alerting</h1>
        <p className="text-xs text-muted-foreground">
          Paste Slack / Discord / generic-JSON webhook URLs. The panel
          POSTs a payload to each URL whenever an alert fires. Payload
          shape is auto-detected from the host:{" "}
          <code className="rounded bg-muted px-1">hooks.slack.com</code>
          {" → "}Slack Block Kit, <code className="rounded bg-muted px-1">
            discord.com/api/webhooks
          </code>{" → "}Discord native, everything else → generic JSON.
        </p>
      </header>

      {loaded.error && (
        <p className="mb-4 rounded border border-rose-500/50 bg-rose-500/10 px-3 py-2 text-sm text-rose-300">
          Failed to load webhooks: {loaded.error.message}
        </p>
      )}

      <section className="mb-4 rounded border bg-card p-4">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Configured endpoints ({draft.length})
        </h2>
        {loaded.isLoading ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : draft.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No webhooks configured. Alerts fire in the UI only.
          </p>
        ) : (
          <ul className="mb-3 divide-y divide-border rounded border bg-background">
            {draft.map((url, idx) => (
              <li
                key={url}
                className="flex items-center justify-between gap-3 px-3 py-2 text-sm"
              >
                <code className="truncate font-mono" title={url}>
                  {url}
                </code>
                <button
                  type="button"
                  onClick={() => removeAt(idx)}
                  aria-label={`Remove ${url}`}
                  className="shrink-0 rounded border px-2 py-0.5 text-xs hover:bg-accent/10"
                >
                  Remove
                </button>
              </li>
            ))}
          </ul>
        )}

        <form
          onSubmit={(e) => {
            e.preventDefault();
            addUrl();
          }}
          className="flex flex-wrap items-center gap-2"
        >
          <input
            value={newUrl}
            onChange={(e) => setNewUrl(e.target.value)}
            placeholder="https://hooks.slack.com/services/…"
            className="flex-1 min-w-[20rem] rounded border bg-background px-2 py-1 font-mono text-sm"
          />
          <button
            type="submit"
            disabled={!newUrl.trim()}
            className="rounded border px-2 py-1 text-xs hover:bg-accent/10 disabled:opacity-50"
          >
            Add
          </button>
        </form>
        <p className="mt-2 text-xs text-muted-foreground">
          Only <code className="font-mono">https</code> URLs are accepted
          (except <code className="font-mono">http://localhost</code> for
          local bridges).
        </p>
      </section>

      <section className="flex flex-wrap items-center gap-3">
        <button
          type="button"
          onClick={onSave}
          disabled={!dirty || save.isPending}
          className="rounded border bg-accent/10 px-3 py-1.5 text-sm hover:bg-accent/20 disabled:opacity-50"
        >
          {save.isPending ? "Saving…" : dirty ? "Save changes" : "Saved"}
        </button>
        {save.error && (
          <span className="text-sm text-rose-400">
            {save.error.message}
          </span>
        )}
      </section>
    </main>
  );
}
