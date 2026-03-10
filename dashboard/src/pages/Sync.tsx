import { useEffect, useState } from "react";
import { sync as syncApi, locks as locksApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { formatDateTime, formatRelativeTime } from "@/lib/utils";
import type { SyncStatus, Lock } from "@/lib/types";

export function Sync() {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [staleLocks, setStaleLocks] = useState<Lock[]>([]);
  const [allLocks, setAllLocks] = useState<Lock[]>([]);
  const [loading, setLoading] = useState(true);
  const [syncing, setSyncing] = useState(false);

  const refresh = () => {
    setLoading(true);
    Promise.all([syncApi.status(), locksApi.list(), locksApi.stale()])
      .then(([s, all, stale]) => {
        setStatus(s);
        setAllLocks(all);
        setStaleLocks(stale);
      })
      .catch(() => {})
      .finally(() => setLoading(false));
  };

  useEffect(refresh, []);

  const handleFetch = async () => {
    setSyncing(true);
    await syncApi.fetch().catch(() => {});
    setSyncing(false);
    refresh();
  };

  const handlePush = async () => {
    setSyncing(true);
    await syncApi.push().catch(() => {});
    setSyncing(false);
    refresh();
  };

  return (
    <div className="p-6 space-y-4 max-w-2xl">
      <h1 className="text-2xl font-bold">Sync</h1>

      <Card>
        <CardHeader>
          <CardTitle className="text-sm flex items-center justify-between">
            Hub Status
            {status?.hub_initialized ? (
              <Badge variant="success">initialized</Badge>
            ) : (
              <Badge variant="secondary">not initialized</Badge>
            )}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 text-sm">
          {loading ? (
            <p className="text-muted-foreground">Loading…</p>
          ) : status ? (
            <>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Remote</span>
                <span className="font-mono text-xs">{status.remote}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Hub Branch</span>
                <span className="font-mono text-xs">{status.hub_branch}</span>
              </div>
              {status.last_fetch_at && (
                <div className="flex justify-between">
                  <span className="text-muted-foreground">Last Fetch</span>
                  <span>{formatRelativeTime(status.last_fetch_at)}</span>
                </div>
              )}
              <div className="flex gap-2 pt-1">
                <Button size="sm" variant="outline" onClick={handleFetch} disabled={syncing}>
                  Fetch
                </Button>
                <Button size="sm" variant="outline" onClick={handlePush} disabled={syncing}>
                  Push
                </Button>
              </div>
            </>
          ) : (
            <p className="text-muted-foreground">Server unavailable.</p>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-sm">
            Locks ({allLocks.length})
            {staleLocks.length > 0 && (
              <Badge variant="destructive" className="ml-2">{staleLocks.length} stale</Badge>
            )}
          </CardTitle>
        </CardHeader>
        <CardContent>
          {allLocks.length === 0 ? (
            <p className="text-muted-foreground text-sm">No active locks.</p>
          ) : (
            <div className="space-y-2">
              {allLocks.map((lock) => (
                <div key={`${lock.issue_id}-${lock.agent_id}`} className="flex items-center justify-between text-sm">
                  <div>
                    <span className="text-blue-400">Issue #{lock.issue_id}</span>
                    <span className="text-muted-foreground ml-2 font-mono text-xs">{lock.agent_id}</span>
                  </div>
                  <div className="flex items-center gap-2">
                    {lock.is_stale && <Badge variant="destructive">stale</Badge>}
                    <span className="text-xs text-muted-foreground">
                      {formatDateTime(lock.claimed_at)}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
