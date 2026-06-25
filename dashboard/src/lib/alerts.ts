// Alert grouping/sorting helpers. Kept out of the page component so
// React Refresh can hot-reload `pages/Alerts.tsx` without hitting the
// "components-only exports" rule.

import type { AlertItem, AlertSeverity } from "@/api/types";

export const SEVERITY_ORDER: AlertSeverity[] = ["critical", "warning", "info"];

/// Group alerts by severity. Within each bucket the rows are ordered
/// newest-first (most recent `opened_at` at index 0).
export function groupBySeverity(rows: AlertItem[]): Record<AlertSeverity, AlertItem[]> {
  const groups: Record<AlertSeverity, AlertItem[]> = { critical: [], warning: [], info: [] };
  for (const row of rows) {
    if (row.severity in groups) {
      groups[row.severity].push(row);
    }
  }
  for (const sev of SEVERITY_ORDER) {
    groups[sev].sort((a, b) => b.opened_at.localeCompare(a.opened_at));
  }
  return groups;
}
