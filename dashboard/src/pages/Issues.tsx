import { useEffect, useState } from "react";
import { Link } from "react-router";
import { Plus, CircleDot, CheckCircle2 } from "lucide-react";
import { useIssuesStore } from "@/stores/issues";
import { issues as issuesApi } from "@/api/client";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { formatRelativeTime } from "@/lib/utils";
import type { IssuePriority } from "@/lib/types";

const PRIORITY_ORDER: Record<IssuePriority, number> = {
  critical: 0, high: 1, medium: 2, low: 3,
};

function priorityVariant(p: IssuePriority) {
  switch (p) {
    case "critical": return "destructive" as const;
    case "high": return "warning" as const;
    case "medium": return "info" as const;
    default: return "secondary" as const;
  }
}

export function Issues() {
  const { issues, loading, fetch } = useIssuesStore();
  const [search, setSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<"open" | "closed" | "all">("open");

  useEffect(() => {
    void fetch({ status: statusFilter === "all" ? undefined : statusFilter });
  }, [fetch, statusFilter]);

  const filtered = issues
    .filter((i) =>
      search === "" ||
      i.title.toLowerCase().includes(search.toLowerCase()) ||
      String(i.id).includes(search),
    )
    .sort((a, b) => PRIORITY_ORDER[a.priority] - PRIORITY_ORDER[b.priority]);

  const handleClose = async (id: number, e: React.MouseEvent) => {
    e.preventDefault();
    await issuesApi.close(id);
    void fetch({ status: statusFilter === "all" ? undefined : statusFilter });
  };

  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Issues</h1>
        <Button size="sm">
          <Plus className="h-4 w-4 mr-1" /> New Issue
        </Button>
      </div>

      <div className="flex items-center gap-3">
        <Input
          placeholder="Search issues…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="max-w-xs"
        />
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
      </div>

      {loading ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : filtered.length === 0 ? (
        <Card>
          <CardContent className="py-10 text-center text-muted-foreground text-sm">
            No issues found.
          </CardContent>
        </Card>
      ) : (
        <div className="space-y-1">
          {filtered.map((issue) => (
            <Link key={issue.id} to={`/issues/${issue.id}`}>
              <div className="flex items-center gap-3 rounded-md border border-border bg-card px-4 py-3 hover:bg-accent/30 transition-colors">
                {issue.status === "open" ? (
                  <CircleDot className="h-4 w-4 text-green-400 shrink-0" />
                ) : (
                  <CheckCircle2 className="h-4 w-4 text-muted-foreground shrink-0" />
                )}
                <span className="font-mono text-xs text-muted-foreground w-8 shrink-0">
                  #{issue.id}
                </span>
                <span className="flex-1 text-sm truncate">{issue.title}</span>
                <Badge variant={priorityVariant(issue.priority)} className="shrink-0">
                  {issue.priority}
                </Badge>
                <span className="text-xs text-muted-foreground shrink-0">
                  {formatRelativeTime(issue.updated_at)}
                </span>
                {issue.status === "open" && (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-6 px-2 text-xs shrink-0"
                    onClick={(e) => handleClose(issue.id, e)}
                  >
                    Close
                  </Button>
                )}
              </div>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
