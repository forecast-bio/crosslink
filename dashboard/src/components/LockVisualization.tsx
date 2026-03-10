import { useState } from "react";
import { Link } from "react-router";
import { Lock, ArrowUpDown, Clock, User } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatRelativeTime } from "@/lib/utils";
import type { LockEntry } from "@/lib/types";

type SortField = "issue_id" | "agent_id" | "age_seconds" | "claimed_at";
type SortDir = "asc" | "desc";

interface Props {
  locks: LockEntry[];
  /** Stale lock timeout in minutes (used for progress bar scaling) */
  staleTimeoutMinutes?: number;
}

function formatAge(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remainMinutes = minutes % 60;
  if (hours < 24) return `${hours}h ${remainMinutes}m`;
  const days = Math.floor(hours / 24);
  return `${days}d ${hours % 24}h`;
}

/**
 * Visual table of all active locks with staleness indicators,
 * sortable columns, and links to issues/agents.
 */
export function LockVisualization({ locks, staleTimeoutMinutes = 60 }: Props) {
  const [sortField, setSortField] = useState<SortField>("age_seconds");
  const [sortDir, setSortDir] = useState<SortDir>("desc");

  const toggleSort = (field: SortField) => {
    if (sortField === field) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortField(field);
      setSortDir("desc");
    }
  };

  const sorted = [...locks].sort((a, b) => {
    const mul = sortDir === "asc" ? 1 : -1;
    switch (sortField) {
      case "issue_id":
        return (a.issue_id - b.issue_id) * mul;
      case "agent_id":
        return a.agent_id.localeCompare(b.agent_id) * mul;
      case "age_seconds":
        return (a.age_seconds - b.age_seconds) * mul;
      case "claimed_at":
        return (new Date(a.claimed_at).getTime() - new Date(b.claimed_at).getTime()) * mul;
      default:
        return 0;
    }
  });

  const staleThresholdSec = staleTimeoutMinutes * 60;

  if (locks.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-10 text-muted-foreground">
        <Lock className="h-8 w-8 mb-2 opacity-40" />
        <p className="text-sm">No active locks</p>
        <p className="text-xs mt-1">Locks are claimed when agents begin working on issues.</p>
      </div>
    );
  }

  return (
    <div className="overflow-x-auto">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>
              <Button variant="ghost" size="sm" className="h-7 px-1 -ml-1" onClick={() => toggleSort("issue_id")}>
                Issue <ArrowUpDown className="ml-1 h-3 w-3" />
              </Button>
            </TableHead>
            <TableHead>
              <Button variant="ghost" size="sm" className="h-7 px-1 -ml-1" onClick={() => toggleSort("agent_id")}>
                Agent <ArrowUpDown className="ml-1 h-3 w-3" />
              </Button>
            </TableHead>
            <TableHead className="hidden sm:table-cell">Branch</TableHead>
            <TableHead>
              <Button variant="ghost" size="sm" className="h-7 px-1 -ml-1" onClick={() => toggleSort("age_seconds")}>
                Age <ArrowUpDown className="ml-1 h-3 w-3" />
              </Button>
            </TableHead>
            <TableHead className="w-[140px]">Staleness</TableHead>
            <TableHead>
              <Button variant="ghost" size="sm" className="h-7 px-1 -ml-1" onClick={() => toggleSort("claimed_at")}>
                Claimed <ArrowUpDown className="ml-1 h-3 w-3" />
              </Button>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {sorted.map((lock) => {
            const stalePct = Math.min((lock.age_seconds / staleThresholdSec) * 100, 100);
            const barColor = lock.is_stale
              ? "bg-red-500"
              : stalePct > 75
                ? "bg-yellow-400"
                : "bg-green-500";

            return (
              <TableRow key={`${lock.issue_id}-${lock.agent_id}`}>
                <TableCell>
                  <Link
                    to={`/issues/${lock.issue_id}`}
                    className="text-blue-400 hover:underline font-mono text-xs"
                  >
                    #{lock.issue_id}
                  </Link>
                </TableCell>
                <TableCell>
                  <Link
                    to={`/agents/${encodeURIComponent(lock.agent_id)}`}
                    className="text-foreground/80 hover:underline font-mono text-xs flex items-center gap-1"
                  >
                    <User className="h-3 w-3 text-muted-foreground" />
                    {lock.agent_id}
                  </Link>
                </TableCell>
                <TableCell className="hidden sm:table-cell">
                  {lock.branch ? (
                    <span className="font-mono text-xs text-muted-foreground truncate max-w-[200px] block">
                      {lock.branch}
                    </span>
                  ) : (
                    <span className="text-xs text-muted-foreground">—</span>
                  )}
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-1.5">
                    <Clock className="h-3 w-3 text-muted-foreground" />
                    <span className="text-xs font-mono">{formatAge(lock.age_seconds)}</span>
                    {lock.is_stale && (
                      <Badge variant="destructive" className="text-[10px] px-1 py-0 leading-tight">
                        STALE
                      </Badge>
                    )}
                  </div>
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-2">
                    <div className="flex-1 h-1.5 bg-muted rounded-full overflow-hidden">
                      <div
                        className={`h-full rounded-full transition-all duration-500 ${barColor}`}
                        style={{ width: `${stalePct}%` }}
                      />
                    </div>
                    <span className="text-[10px] text-muted-foreground w-8 text-right">
                      {Math.round(stalePct)}%
                    </span>
                  </div>
                </TableCell>
                <TableCell>
                  <span className="text-xs text-muted-foreground">
                    {formatRelativeTime(lock.claimed_at)}
                  </span>
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </div>
  );
}
