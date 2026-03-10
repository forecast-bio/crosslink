import { useState } from "react";
import { Link } from "react-router";
import { CircleDot, CheckCircle2, ChevronUp, ChevronDown, ChevronsUpDown } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { formatRelativeTime } from "@/lib/utils";
import type { Issue, IssuePriority } from "@/lib/types";

type SortKey = "id" | "title" | "priority" | "updated_at";
type SortDir = "asc" | "desc";

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

function SortIcon({ col, sort }: { col: SortKey; sort: { key: SortKey; dir: SortDir } }) {
  if (sort.key !== col) return <ChevronsUpDown className="h-3 w-3 ml-1 opacity-40" />;
  return sort.dir === "asc"
    ? <ChevronUp className="h-3 w-3 ml-1" />
    : <ChevronDown className="h-3 w-3 ml-1" />;
}

interface IssueTableProps {
  issues: Issue[];
  onToggleStatus: (id: number, currentStatus: string) => Promise<void>;
}

export function IssueTable({ issues, onToggleStatus }: IssueTableProps) {
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({
    key: "updated_at",
    dir: "desc",
  });

  function toggleSort(key: SortKey) {
    setSort((s) =>
      s.key === key ? { key, dir: s.dir === "asc" ? "desc" : "asc" } : { key, dir: "asc" },
    );
  }

  const sorted = [...issues].sort((a, b) => {
    let cmp = 0;
    switch (sort.key) {
      case "id":
        cmp = a.id - b.id;
        break;
      case "title":
        cmp = a.title.localeCompare(b.title);
        break;
      case "priority":
        cmp = PRIORITY_ORDER[a.priority] - PRIORITY_ORDER[b.priority];
        break;
      case "updated_at":
        cmp = new Date(a.updated_at).getTime() - new Date(b.updated_at).getTime();
        break;
    }
    return sort.dir === "asc" ? cmp : -cmp;
  });

  if (sorted.length === 0) {
    return (
      <div className="rounded-md border border-border py-12 text-center text-sm text-muted-foreground">
        No issues found.
      </div>
    );
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead className="w-8" />
          <TableHead className="w-16">
            <button
              className="flex items-center font-medium hover:text-foreground transition-colors"
              onClick={() => toggleSort("id")}
            >
              # <SortIcon col="id" sort={sort} />
            </button>
          </TableHead>
          <TableHead>
            <button
              className="flex items-center font-medium hover:text-foreground transition-colors"
              onClick={() => toggleSort("title")}
            >
              Title <SortIcon col="title" sort={sort} />
            </button>
          </TableHead>
          <TableHead className="hidden md:table-cell">Labels</TableHead>
          <TableHead className="w-24">
            <button
              className="flex items-center font-medium hover:text-foreground transition-colors"
              onClick={() => toggleSort("priority")}
            >
              Priority <SortIcon col="priority" sort={sort} />
            </button>
          </TableHead>
          <TableHead className="w-28">
            <button
              className="flex items-center font-medium hover:text-foreground transition-colors"
              onClick={() => toggleSort("updated_at")}
            >
              Updated <SortIcon col="updated_at" sort={sort} />
            </button>
          </TableHead>
          <TableHead className="w-20" />
        </TableRow>
      </TableHeader>
      <TableBody>
        {sorted.map((issue) => {
          const labels = "labels" in issue ? (issue as Issue & { labels?: string[] }).labels ?? [] : [];
          return (
            <TableRow key={issue.id} className="group">
              <TableCell className="px-2">
                {issue.status === "open" ? (
                  <CircleDot className="h-4 w-4 text-green-400" />
                ) : (
                  <CheckCircle2 className="h-4 w-4 text-muted-foreground" />
                )}
              </TableCell>
              <TableCell className="font-mono text-xs text-muted-foreground px-2">
                #{issue.id}
              </TableCell>
              <TableCell>
                <Link
                  to={`/issues/${issue.id}`}
                  className="hover:text-blue-400 transition-colors font-medium"
                >
                  {issue.title}
                </Link>
              </TableCell>
              <TableCell className="hidden md:table-cell">
                <div className="flex flex-wrap gap-1">
                  {labels.map((l) => (
                    <Badge key={l} variant="secondary" className="text-xs">
                      {l}
                    </Badge>
                  ))}
                </div>
              </TableCell>
              <TableCell>
                <Badge variant={priorityVariant(issue.priority)}>{issue.priority}</Badge>
              </TableCell>
              <TableCell className="text-xs text-muted-foreground">
                {formatRelativeTime(issue.updated_at)}
              </TableCell>
              <TableCell className="text-right pr-2">
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-6 px-2 text-xs opacity-0 group-hover:opacity-100 transition-opacity"
                  onClick={(e) => {
                    e.preventDefault();
                    void onToggleStatus(issue.id, issue.status);
                  }}
                >
                  {issue.status === "open" ? "Close" : "Reopen"}
                </Button>
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
