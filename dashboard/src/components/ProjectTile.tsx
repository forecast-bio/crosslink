// SCADA-style project tile. Dense, colour-coded, scannable at-a-glance.
// Design §8 "Tile anatomy" for the intended layout.

import { Link } from "react-router-dom";

import type { ProjectListItem, WriteCapability } from "@/api/types";
import { tileSeverity, type TileSeverity } from "@/lib/severity";

const WRITE_CAP_LABEL: Record<Exclude<WriteCapability, "ready">, string> = {
  not_initialized: "not initialized",
  agent_missing: "agent missing",
};

const SEVERITY_BORDER: Record<TileSeverity, string> = {
  nominal: "border-emerald-500/50",
  warning: "border-amber-500/70",
  critical: "border-rose-500/80 ring-2 ring-rose-500/30",
  paused: "border-muted-foreground/30",
  unreachable: "border-muted-foreground/30 opacity-60",
};

const SEVERITY_DOT: Record<TileSeverity, string> = {
  nominal: "bg-emerald-500",
  warning: "bg-amber-500",
  critical: "bg-rose-500",
  paused: "bg-muted-foreground/50",
  unreachable: "bg-muted-foreground/50",
};

function relativeTime(iso: string | null): string {
  if (!iso) return "—";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return iso;
  const deltaMs = Date.now() - then;
  const s = Math.floor(deltaMs / 1000);
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  return `${d}d ago`;
}

export function ProjectTile({ item }: { item: ProjectListItem }) {
  const severity = tileSeverity(item);
  const c = item.counters;

  return (
    <Link
      to={`/project/${item.slug}`}
      className={`block rounded-md border bg-card text-card-foreground p-3 transition hover:bg-accent/5 ${SEVERITY_BORDER[severity]}`}
    >
      <div className="flex items-center gap-2 pb-2">
        <span className={`h-2 w-2 shrink-0 rounded-full ${SEVERITY_DOT[severity]}`} aria-hidden />
        <span className="truncate text-sm font-medium">{item.slug}</span>
        {item.pinned && <span className="text-xs opacity-70" aria-label="pinned">📌</span>}
        {item.write_capability !== "ready" && (
          <span
            title="Dashboard write actions will fail. Run `crosslink init` + `crosslink agent init` in this workspace, or click the Initialize button on the project detail page."
            className="ml-auto shrink-0 rounded border border-amber-500/60 bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-amber-500"
          >
            ⚠ {WRITE_CAP_LABEL[item.write_capability]}
          </span>
        )}
      </div>
      <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <div className="flex items-baseline justify-between">
          <dt>Open</dt>
          <dd className="text-foreground tabular-nums">{c.open_issues}</dd>
        </div>
        <div className="flex items-baseline justify-between">
          <dt>Overdue</dt>
          <dd className={`tabular-nums ${c.overdue_issues > 0 ? "text-rose-500 font-medium" : "text-foreground"}`}>
            {c.overdue_issues}
          </dd>
        </div>
        <div className="flex items-baseline justify-between">
          <dt>Agents</dt>
          <dd className="text-foreground tabular-nums">{c.active_agents}</dd>
        </div>
        <div className="flex items-baseline justify-between">
          <dt>Stale locks</dt>
          <dd className={`tabular-nums ${c.stale_locks > 0 ? "text-rose-500 font-medium" : "text-foreground"}`}>
            {c.stale_locks}
          </dd>
        </div>
      </dl>
      <div className="mt-2 border-t pt-2 text-xs text-muted-foreground">
        <span>Last activity: {relativeTime(item.last_activity_at)}</span>
      </div>
    </Link>
  );
}

