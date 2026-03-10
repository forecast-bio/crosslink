import { Link } from "react-router";
import { Lock } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import type { AgentLockEntry } from "@/lib/types";

interface Props {
  locks: AgentLockEntry[];
}

/**
 * Displays the list of issue locks currently held by an agent.
 * Each entry links to the corresponding issue detail page.
 */
export function LockList({ locks }: Props) {
  if (locks.length === 0) {
    return (
      <p className="text-sm text-muted-foreground flex items-center gap-1.5">
        <Lock className="h-3.5 w-3.5" />
        No locks held
      </p>
    );
  }

  return (
    <div className="space-y-2">
      {locks.map((lock) => {
        const heldMinutes = Math.round(lock.age_seconds / 60);
        const heldDisplay =
          heldMinutes >= 60
            ? `${Math.floor(heldMinutes / 60)}h ${heldMinutes % 60}m`
            : `${heldMinutes}m`;

        return (
          <div
            key={lock.issue_id}
            className="flex items-center justify-between text-sm py-1 border-b border-border last:border-0"
          >
            <Link
              to={`/issues/${lock.issue_id}`}
              className="text-blue-400 hover:underline font-mono text-xs"
            >
              #{lock.issue_id}
            </Link>
            <div className="flex items-center gap-2">
              {lock.stale && (
                <Badge variant="destructive" className="text-xs px-1.5 py-0">
                  stale
                </Badge>
              )}
              <span className="text-xs text-muted-foreground">held {heldDisplay}</span>
              <span className="text-xs text-muted-foreground hidden sm:block">
                since{" "}
                {new Date(lock.claimed_at).toLocaleTimeString([], {
                  hour: "2-digit",
                  minute: "2-digit",
                })}
              </span>
            </div>
          </div>
        );
      })}
    </div>
  );
}
