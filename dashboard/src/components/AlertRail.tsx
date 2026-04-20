// Compact alert banner that lives at the top of every page. Shows an
// aggregate count of currently-open alerts bucketed by severity, with
// a click-through to the full /alerts page. Invisible when everything
// is nominal.

import { Link } from "react-router-dom";

import { useAlerts } from "@/api/client";

const SEVERITY_ORDER = ["critical", "warning", "info"] as const;

const SEVERITY_CLASSES: Record<(typeof SEVERITY_ORDER)[number], string> = {
  critical: "bg-rose-500/15 text-rose-500 border-rose-500/50",
  warning: "bg-amber-500/15 text-amber-500 border-amber-500/50",
  info: "bg-sky-500/15 text-sky-500 border-sky-500/50",
};

export function AlertRail() {
  const { data, isLoading } = useAlerts();

  if (isLoading || !data || data.length === 0) {
    return null;
  }

  const counts: Record<(typeof SEVERITY_ORDER)[number], number> = {
    critical: 0,
    warning: 0,
    info: 0,
  };
  for (const alert of data) {
    if (alert.severity in counts) {
      counts[alert.severity] += 1;
    }
  }

  const hasAny = SEVERITY_ORDER.some((s) => counts[s] > 0);
  if (!hasAny) return null;

  return (
    <Link
      to="/alerts"
      className="block border-b border-border bg-card/80 backdrop-blur hover:bg-card"
    >
      <div className="mx-auto flex max-w-6xl items-center gap-3 px-6 py-2 text-sm">
        <span className="font-medium">Alerts:</span>
        {SEVERITY_ORDER.map((sev) =>
          counts[sev] > 0 ? (
            <span
              key={sev}
              className={`rounded-full border px-2 py-0.5 text-xs font-medium tabular-nums ${SEVERITY_CLASSES[sev]}`}
            >
              {counts[sev]} {sev}
            </span>
          ) : null,
        )}
        <span className="ml-auto text-xs text-muted-foreground">view all →</span>
      </div>
    </Link>
  );
}
