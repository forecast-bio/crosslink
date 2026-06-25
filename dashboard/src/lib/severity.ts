// Tile severity derivation, kept out of the component file so React
// Refresh can hot-reload `ProjectTile.tsx` without tripping its
// "components-only exports" rule.

import type { ProjectListItem } from "@/api/types";

export type TileSeverity = "nominal" | "warning" | "critical" | "paused" | "unreachable";

/// Derive the overall tile colour from alert-shaped signals in the
/// counters. The real alert engine lands in P1.6 — until then, tile
/// colour is a conservative approximation based on what the index
/// already exposes.
export function tileSeverity(item: ProjectListItem): TileSeverity {
  if (item.status === "paused") return "paused";
  if (item.status === "error") return "unreachable";
  const c = item.counters;
  if (c.stale_locks > 0) return "critical";
  if (c.overdue_issues > 0 || c.blocked_issues > 0 || c.ci_status === "failing") {
    return "warning";
  }
  return "nominal";
}
