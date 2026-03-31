import { useEffect, useState } from "react";
import { sync as syncApi, locks as locksApi, config as configApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import {
  RefreshCw,
  GitBranch,
  ArrowDownToLine,
  ArrowUpFromLine,
  Database,
  Lock as LockIcon,
  AlertTriangle,
} from "lucide-react";
import { formatRelativeTime } from "@/lib/utils";
import { LockVisualization } from "@/components/LockVisualization";
import type { SyncStatus, LockEntry } from "@/lib/types";

export function Sync() {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [staleLocks, setStaleLocks] = useState<LockEntry[]>([]);
  const [allLocks, setAllLocks] = useState<LockEntry[]>([]);
  const [staleTimeoutMinutes, setStaleTimeoutMinutes] = useState(60);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState<"fetch" | "push" | null>(null);
  const [lastRefresh, setLastRefresh] = useState<Date | null>(null);

  const refresh = () => {
    setLoading(true);
    setError(null);
    Promise.all([
      syncApi.status(),
      locksApi.list(),
      locksApi.stale(),
      configApi.get(),
    ])
      .then(([s, all, stale, cfg]) => {
        setStatus(s);
        setAllLocks(all);
        setStaleLocks(stale);
        setStaleTimeoutMinutes(cfg.stale_lock_timeout_minutes);
        setLastRefresh(new Date());
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  };

  useEffect(() => { refresh(); }, []);

  const handleFetch = async () => {
    setSyncing("fetch");
    try {
      await syncApi.fetch();
    } catch (e) {
      setError(String(e));
    }
    setSyncing(null);
    refresh();
  };

  const handlePush = async () => {
    setSyncing("push");
    try {
      await syncApi.push();
    } catch (e) {
      setError(String(e));
    }
    setSyncing(null);
    refresh();
  };

  const activeLockCount = allLocks.filter((l) => !l.is_stale).length;
  const staleLockCount = staleLocks.length;

  return (
    <div className="p-6 space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Sync</h1>
        <div className="flex items-center gap-2">
          {lastRefresh && (
            <span className="text-xs text-muted-foreground">
              Updated {formatRelativeTime(lastRefresh.toISOString())}
            </span>
          )}
          <Button variant="ghost" size="sm" onClick={refresh} disabled={loading}>
            <RefreshCw className={`h-4 w-4 ${loading ? "animate-spin" : ""}`} />
          </Button>
        </div>
      </div>

      {error && (
        <p className="text-destructive text-sm">{error}</p>
      )}

      {/* Hub status + sync actions */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        {/* Hub status card */}
        <Card className="lg:col-span-2">
          <CardHeader className="pb-2">
            <CardTitle className="text-sm flex items-center justify-between">
              <span className="flex items-center gap-2">
                <Database className="h-4 w-4" />
                Hub Status
              </span>
              {status?.hub_initialized ? (
                <Badge variant="success">initialized</Badge>
              ) : status !== null ? (
                <Badge variant="secondary">not initialized</Badge>
              ) : null}
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-3 text-sm">
            {loading && !status ? (
              <p className="text-muted-foreground">Loading…</p>
            ) : status ? (
              <>
                <div className="grid grid-cols-2 gap-x-6 gap-y-2">
                  <div className="flex justify-between">
                    <span className="text-muted-foreground">Remote</span>
                    <span className="font-mono text-xs">{status.remote}</span>
                  </div>
                  <div className="flex justify-between">
                    <span className="text-muted-foreground">Hub Branch</span>
                    <span className="font-mono text-xs flex items-center gap-1">
                      <GitBranch className="h-3 w-3" />
                      {status.hub_branch}
                    </span>
                  </div>
                  <div className="flex justify-between">
                    <span className="text-muted-foreground">Active Locks</span>
                    <span className="font-mono text-xs">{status.active_lock_count}</span>
                  </div>
                  <div className="flex justify-between">
                    <span className="text-muted-foreground">Stale Locks</span>
                    <span className="font-mono text-xs">
                      {status.stale_lock_count > 0 ? (
                        <span className="text-red-400">{status.stale_lock_count}</span>
                      ) : (
                        "0"
                      )}
                    </span>
                  </div>
                  {status.last_fetch_at && (
                    <div className="flex justify-between col-span-2">
                      <span className="text-muted-foreground">Last Fetch</span>
                      <span>{formatRelativeTime(status.last_fetch_at)}</span>
                    </div>
                  )}
                </div>
              </>
            ) : (
              <p className="text-muted-foreground">Server unavailable.</p>
            )}
          </CardContent>
        </Card>

        {/* Sync actions card */}
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm">Sync Actions</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <Button
              className="w-full justify-start"
              variant="outline"
              size="sm"
              onClick={handleFetch}
              disabled={syncing !== null}
            >
              <ArrowDownToLine className={`h-4 w-4 mr-2 ${syncing === "fetch" ? "animate-bounce" : ""}`} />
              {syncing === "fetch" ? "Fetching…" : "Fetch from remote"}
            </Button>
            <Button
              className="w-full justify-start"
              variant="outline"
              size="sm"
              onClick={handlePush}
              disabled={syncing !== null}
            >
              <ArrowUpFromLine className={`h-4 w-4 mr-2 ${syncing === "push" ? "animate-bounce" : ""}`} />
              {syncing === "push" ? "Pushing…" : "Push to remote"}
            </Button>
            <Separator />
            <p className="text-[11px] text-muted-foreground leading-relaxed">
              Fetch pulls heartbeats, locks, and keyring updates from the hub branch.
              Push sends local changes to the remote.
            </p>
          </CardContent>
        </Card>
      </div>

      {/* Lock overview stats */}
      {!loading && (
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
          <StatCard label="Total Locks" value={allLocks.length} icon={LockIcon} />
          <StatCard label="Active" value={activeLockCount} icon={LockIcon} variant="success" />
          <StatCard
            label="Stale"
            value={staleLockCount}
            icon={AlertTriangle}
            variant={staleLockCount > 0 ? "destructive" : "secondary"}
          />
          <StatCard
            label="Stale Timeout"
            value={`${staleTimeoutMinutes}m`}
            icon={RefreshCw}
          />
        </div>
      )}

      {/* Lock visualization table */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm flex items-center justify-between">
            <span className="flex items-center gap-2">
              <LockIcon className="h-4 w-4" />
              Lock Table
            </span>
            <div className="flex items-center gap-2">
              {staleLockCount > 0 && (
                <Badge variant="destructive">{staleLockCount} stale</Badge>
              )}
              <Badge variant="outline">{allLocks.length} total</Badge>
            </div>
          </CardTitle>
        </CardHeader>
        <CardContent>
          <LockVisualization
            locks={allLocks}
            staleTimeoutMinutes={staleTimeoutMinutes}
          />
        </CardContent>
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Small stat card used in the overview strip
// ---------------------------------------------------------------------------

function StatCard({
  label,
  value,
  icon: Icon,
  variant,
}: {
  label: string;
  value: number | string;
  icon: React.ComponentType<{ className?: string }>;
  variant?: "success" | "destructive" | "secondary";
}) {
  const color =
    variant === "success"
      ? "text-green-400"
      : variant === "destructive"
        ? "text-red-400"
        : "text-muted-foreground";

  return (
    <Card>
      <CardContent className="p-4 flex items-center gap-3">
        <Icon className={`h-5 w-5 ${color}`} />
        <div>
          <p className="text-lg font-semibold leading-none">{value}</p>
          <p className="text-xs text-muted-foreground mt-0.5">{label}</p>
        </div>
      </CardContent>
    </Card>
  );
}
