// Full alerts page. Shows every currently-open alert across all
// tracked projects, grouped by severity then ordered by opened_at.
// Future P3 polish: ack / dismiss per row, severity filter chips,
// project-scoped filter.

import { Link } from "react-router-dom";

import { useAlerts } from "@/api/client";
import type { AlertSeverity } from "@/api/types";
import { groupBySeverity, SEVERITY_ORDER } from "@/lib/alerts";

const SEVERITY_CLASSES: Record<AlertSeverity, string> = {
  critical: "border-rose-500/60 bg-rose-500/10",
  warning: "border-amber-500/60 bg-amber-500/10",
  info: "border-sky-500/50 bg-sky-500/10",
};

const SEVERITY_BADGE: Record<AlertSeverity, string> = {
  critical: "bg-rose-500 text-white",
  warning: "bg-amber-500 text-white",
  info: "bg-sky-500 text-white",
};

export function Alerts() {
  const { data, isLoading, error } = useAlerts();

  if (isLoading) {
    return (
      <main className="mx-auto max-w-6xl px-6 py-8">
        <p className="text-muted-foreground">Loading alerts…</p>
      </main>
    );
  }

  if (error) {
    return (
      <main className="mx-auto max-w-6xl px-6 py-8">
        <p className="text-rose-500">Failed to load alerts: {error.message}</p>
      </main>
    );
  }

  const rows = data ?? [];
  const groups = groupBySeverity(rows);

  return (
    <main className="mx-auto max-w-6xl px-6 py-6">
      <nav className="mb-4 text-sm">
        <Link to="/" className="text-muted-foreground hover:underline">
          ← All projects
        </Link>
      </nav>
      <header className="mb-6 flex items-baseline justify-between">
        <h1 className="text-xl font-semibold">Alerts</h1>
        <span className="text-xs text-muted-foreground tabular-nums">
          {rows.length} open
        </span>
      </header>

      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">No open alerts — all clear.</p>
      ) : (
        SEVERITY_ORDER.map((sev) =>
          groups[sev].length > 0 ? (
            <section key={sev} className="mb-6">
              <h2 className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                {sev} ({groups[sev].length})
              </h2>
              <ul className="space-y-2">
                {groups[sev].map((alert) => (
                  <li
                    key={alert.id}
                    className={`rounded border p-3 ${SEVERITY_CLASSES[sev]}`}
                  >
                    <div className="flex items-baseline justify-between gap-3">
                      <div>
                        <span
                          className={`mr-2 inline-block rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${SEVERITY_BADGE[sev]}`}
                        >
                          {alert.kind.replace(/_/g, " ")}
                        </span>
                        <Link
                          to={`/project/${alert.project_slug}`}
                          className="text-sm font-medium hover:underline"
                        >
                          {alert.project_slug}
                        </Link>
                        {alert.subject_ref && (
                          <span className="ml-2 text-xs text-muted-foreground">
                            {alert.subject_ref}
                          </span>
                        )}
                      </div>
                      <span className="text-xs text-muted-foreground tabular-nums">
                        opened {alert.opened_at}
                      </span>
                    </div>
                    {alert.detail && (
                      <p className="mt-1 text-sm text-muted-foreground">{alert.detail}</p>
                    )}
                  </li>
                ))}
              </ul>
            </section>
          ) : null,
        )
      )}
    </main>
  );
}

