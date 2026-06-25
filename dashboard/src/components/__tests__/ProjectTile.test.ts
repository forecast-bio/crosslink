import { describe, expect, it } from "vitest";

import { tileSeverity } from "@/lib/severity";
import type { ProjectListItem } from "@/api/types";

function make(partial: Partial<ProjectListItem["counters"]> & { status?: string }): ProjectListItem {
  const { status = "active", ...counters } = partial;
  return {
    slug: "owner/repo",
    status,
    pinned: false,
    hub_sha: null,
    hub_fetched_at: null,
    last_activity_at: null,
    added_at: "2026-04-20T00:00:00Z",
    write_capability: "ready",
    counters: {
      open_issues: 0,
      overdue_issues: 0,
      due_soon_issues: 0,
      blocked_issues: 0,
      active_agents: 0,
      stale_locks: 0,
      ci_status: null,
      updated_at: null,
      ...counters,
    },
  };
}

describe("tileSeverity", () => {
  it("returns 'nominal' for an all-zero project", () => {
    expect(tileSeverity(make({}))).toBe("nominal");
  });

  it("returns 'critical' when any stale lock is present", () => {
    expect(tileSeverity(make({ stale_locks: 1 }))).toBe("critical");
  });

  it("returns 'warning' on overdue issues without stale locks", () => {
    expect(tileSeverity(make({ overdue_issues: 2 }))).toBe("warning");
  });

  it("returns 'warning' on blocked issues", () => {
    expect(tileSeverity(make({ blocked_issues: 1 }))).toBe("warning");
  });

  it("returns 'warning' when CI is failing", () => {
    expect(tileSeverity(make({ ci_status: "failing" }))).toBe("warning");
  });

  it("returns 'paused' for projects with status=paused", () => {
    expect(tileSeverity(make({ status: "paused" }))).toBe("paused");
  });

  it("returns 'unreachable' for projects in status=error", () => {
    expect(tileSeverity(make({ status: "error" }))).toBe("unreachable");
  });

  it("critical beats warning — stale lock + overdue", () => {
    expect(
      tileSeverity(make({ stale_locks: 1, overdue_issues: 3 })),
    ).toBe("critical");
  });
});
