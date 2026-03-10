import { useEffect, useState } from "react";
import { sessions as sessionsApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { formatDateTime, formatRelativeTime } from "@/lib/utils";
import type { Session } from "@/lib/types";

export function Sessions() {
  const [current, setCurrent] = useState<Session | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = () => {
    setLoading(true);
    sessionsApi
      .current()
      .then(setCurrent)
      .catch(() => setCurrent(null))
      .finally(() => setLoading(false));
  };

  useEffect(refresh, []);

  const handleStart = async () => {
    await sessionsApi.start();
    refresh();
  };

  const handleEnd = async () => {
    await sessionsApi.end();
    refresh();
  };

  return (
    <div className="p-6 space-y-4 max-w-xl">
      <h1 className="text-2xl font-bold">Sessions</h1>

      <Card>
        <CardHeader>
          <CardTitle className="text-sm flex items-center justify-between">
            Current Session
            {current ? (
              <Badge variant="success">active</Badge>
            ) : (
              <Badge variant="secondary">none</Badge>
            )}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 text-sm">
          {loading ? (
            <p className="text-muted-foreground">Loading…</p>
          ) : current ? (
            <>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Agent</span>
                <span className="font-mono text-xs">{current.agent_id}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Started</span>
                <span>{formatRelativeTime(current.started_at)}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Exact</span>
                <span className="text-xs text-muted-foreground">{formatDateTime(current.started_at)}</span>
              </div>
              {current.active_issue_id && (
                <div className="flex justify-between">
                  <span className="text-muted-foreground">Working on</span>
                  <span className="text-blue-400">Issue #{current.active_issue_id}</span>
                </div>
              )}
              {current.handoff_notes && (
                <p className="text-xs text-muted-foreground border-t border-border pt-2">
                  {current.handoff_notes}
                </p>
              )}
              <Button size="sm" variant="outline" onClick={handleEnd}>
                End Session
              </Button>
            </>
          ) : (
            <>
              <p className="text-muted-foreground">No active session.</p>
              <Button size="sm" onClick={handleStart}>
                Start Session
              </Button>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
