import { describe, expect, it } from "vitest";

import type { AlertItem, AlertSeverity } from "@/api/types";
import { groupBySeverity, SEVERITY_ORDER } from "@/lib/alerts";

function mk(
  id: number,
  severity: AlertSeverity,
  opened_at: string,
): AlertItem {
  return {
    id,
    project_slug: "owner/repo",
    kind: "stale_lock",
    severity,
    subject_ref: null,
    detail: null,
    opened_at,
    resolved_at: null,
    acknowledged_at: null,
  };
}

describe("groupBySeverity", () => {
  it("buckets alerts by severity", () => {
    const out = groupBySeverity([
      mk(1, "critical", "2026-04-20T01:00:00Z"),
      mk(2, "warning", "2026-04-20T02:00:00Z"),
      mk(3, "info", "2026-04-20T03:00:00Z"),
    ]);
    expect(out.critical).toHaveLength(1);
    expect(out.warning).toHaveLength(1);
    expect(out.info).toHaveLength(1);
  });

  it("sorts each bucket newest-first", () => {
    const out = groupBySeverity([
      mk(1, "warning", "2026-04-20T01:00:00Z"),
      mk(2, "warning", "2026-04-20T03:00:00Z"),
      mk(3, "warning", "2026-04-20T02:00:00Z"),
    ]);
    expect(out.warning.map((a) => a.id)).toEqual([2, 3, 1]);
  });

  it("returns empty arrays for severities with no alerts", () => {
    const out = groupBySeverity([mk(1, "critical", "2026-04-20T00:00:00Z")]);
    expect(out.warning).toEqual([]);
    expect(out.info).toEqual([]);
  });

  it("preserves the documented severity order", () => {
    expect(SEVERITY_ORDER).toEqual(["critical", "warning", "info"]);
  });
});
