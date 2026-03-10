import { useEffect, useState } from "react";
import { Plus } from "lucide-react";
import { useIssuesStore } from "@/stores/issues";
import { issues as issuesApi } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { IssueTable } from "@/components/IssueTable";
import { IssueForm } from "@/components/IssueForm";
import type { IssuePriority } from "@/lib/types";

const PRIORITIES: IssuePriority[] = ["critical", "high", "medium", "low"];

export function Issues() {
  const { issues, loading, fetch, create } = useIssuesStore();
  const [search, setSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<"open" | "closed" | "all">("open");
  const [priorityFilter, setPriorityFilter] = useState<IssuePriority | "all">("all");
  const [labelFilter, setLabelFilter] = useState("");
  const [showForm, setShowForm] = useState(false);

  useEffect(() => {
    void fetch({
      status: statusFilter === "all" ? undefined : statusFilter,
      priority: priorityFilter === "all" ? undefined : priorityFilter,
      label: labelFilter || undefined,
    });
  }, [fetch, statusFilter, priorityFilter, labelFilter]);

  // Client-side search filter (supplements server-side when search is typed)
  const filtered = search
    ? issues.filter(
        (i) =>
          i.title.toLowerCase().includes(search.toLowerCase()) ||
          String(i.id).includes(search),
      )
    : issues;

  const handleToggleStatus = async (id: number, currentStatus: string) => {
    if (currentStatus === "open") {
      await issuesApi.close(id);
    } else {
      await issuesApi.reopen(id);
    }
    void fetch({
      status: statusFilter === "all" ? undefined : statusFilter,
      priority: priorityFilter === "all" ? undefined : priorityFilter,
      label: labelFilter || undefined,
    });
  };

  // Collect unique labels from loaded issues for the filter chips
  const allLabels = Array.from(
    new Set(
      issues.flatMap((i) =>
        "labels" in i ? ((i as typeof i & { labels?: string[] }).labels ?? []) : [],
      ),
    ),
  ).sort();

  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Issues</h1>
        <Button size="sm" onClick={() => setShowForm(true)}>
          <Plus className="h-4 w-4 mr-1" /> New Issue
        </Button>
      </div>

      {/* Filters row */}
      <div className="flex flex-wrap items-center gap-3">
        <Input
          placeholder="Search issues…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="max-w-xs"
        />

        {/* Status filter */}
        <div className="flex gap-1">
          {(["open", "closed", "all"] as const).map((s) => (
            <Button
              key={s}
              size="sm"
              variant={statusFilter === s ? "secondary" : "ghost"}
              onClick={() => setStatusFilter(s)}
              className="capitalize"
            >
              {s}
            </Button>
          ))}
        </div>

        {/* Priority filter */}
        <div className="flex gap-1">
          <Button
            size="sm"
            variant={priorityFilter === "all" ? "secondary" : "ghost"}
            onClick={() => setPriorityFilter("all")}
          >
            All priorities
          </Button>
          {PRIORITIES.map((p) => (
            <Button
              key={p}
              size="sm"
              variant={priorityFilter === p ? "secondary" : "ghost"}
              onClick={() => setPriorityFilter(p)}
              className="capitalize"
            >
              {p}
            </Button>
          ))}
        </div>
      </div>

      {/* Label filter chips */}
      {allLabels.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {labelFilter && (
            <button
              className="px-2 py-0.5 rounded-full text-xs bg-secondary text-secondary-foreground hover:bg-secondary/70 transition-colors"
              onClick={() => setLabelFilter("")}
            >
              Clear label ×
            </button>
          )}
          {allLabels.map((l) => (
            <button
              key={l}
              onClick={() => setLabelFilter(labelFilter === l ? "" : l)}
              className={`px-2 py-0.5 rounded-full text-xs border transition-colors ${
                labelFilter === l
                  ? "bg-primary text-primary-foreground border-primary"
                  : "border-border text-muted-foreground hover:border-foreground hover:text-foreground"
              }`}
            >
              {l}
            </button>
          ))}
        </div>
      )}

      {loading ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : (
        <IssueTable issues={filtered} onToggleStatus={handleToggleStatus} />
      )}

      <IssueForm
        open={showForm}
        onOpenChange={setShowForm}
        onSubmit={async (data) => {
          await create(data);
        }}
      />
    </div>
  );
}
