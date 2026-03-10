import { useEffect, useState } from "react";
import { Link } from "react-router";
import { Activity, Play, Square } from "lucide-react";
import { sessions as sessionsApi } from "@/api/client";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatRelativeTime } from "@/lib/utils";
import type { Session } from "@/lib/types";

/**
 * Compact session status widget — shown in the sidebar or page headers.
 * Displays current session state and provides start/end buttons.
 */
export function SessionPanel() {
  const [current, setCurrent] = useState<Session | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = () => {
    sessionsApi
      .current()
      .then(setCurrent)
      .catch(() => setCurrent(null))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 30_000);
    return () => clearInterval(interval);
  }, []);

  const handleStart = async () => {
    setLoading(true);
    await sessionsApi.start();
    refresh();
  };

  const handleEnd = async () => {
    setLoading(true);
    await sessionsApi.end();
    refresh();
  };

  if (loading) {
    return (
      <div className="flex items-center gap-2 px-3 py-2 text-xs text-muted-foreground">
        <Activity className="h-3 w-3 animate-pulse" />
        <span>Session…</span>
      </div>
    );
  }

  if (!current) {
    return (
      <div className="flex items-center gap-2 px-3 py-2">
        <Activity className="h-3 w-3 text-muted-foreground" />
        <span className="text-xs text-muted-foreground flex-1">No session</span>
        <Button size="sm" variant="ghost" className="h-5 px-2 text-xs" onClick={handleStart}>
          <Play className="h-3 w-3 mr-1" />
          Start
        </Button>
      </div>
    );
  }

  return (
    <div className="px-3 py-2 space-y-1">
      <div className="flex items-center gap-2">
        <Activity className="h-3 w-3 text-green-400 shrink-0" />
        <Badge variant="success" className="text-xs h-4 px-1.5">active</Badge>
        <span className="text-xs text-muted-foreground flex-1 truncate">
          {formatRelativeTime(current.started_at)}
        </span>
        <Button size="sm" variant="ghost" className="h-5 px-2 text-xs shrink-0" onClick={handleEnd}>
          <Square className="h-3 w-3 mr-1" />
          End
        </Button>
      </div>
      {current.active_issue_id && (
        <div className="pl-5 text-xs text-muted-foreground">
          Working on{" "}
          <Link
            to={`/issues/${current.active_issue_id}`}
            className="text-blue-400 hover:underline"
          >
            #{current.active_issue_id}
          </Link>
        </div>
      )}
    </div>
  );
}
