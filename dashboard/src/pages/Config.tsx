import { useEffect, useState } from "react";
import { config as configApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import type { Config } from "@/lib/types";

export function Config() {
  const [cfg, setCfg] = useState<Config | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [trackingMode, setTrackingMode] = useState<Config["tracking_mode"]>("normal");

  useEffect(() => {
    configApi
      .get()
      .then((c) => {
        setCfg(c);
        setTrackingMode(c.tracking_mode);
      })
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const handleSave = async () => {
    setSaving(true);
    const updated = await configApi.update({ tracking_mode: trackingMode }).catch(() => null);
    if (updated) setCfg(updated);
    setSaving(false);
  };

  return (
    <div className="p-6 space-y-4 max-w-xl">
      <h1 className="text-2xl font-bold">Config</h1>

      {loading ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : !cfg ? (
        <p className="text-muted-foreground text-sm">Server unavailable.</p>
      ) : (
        <>
          <Card>
            <CardHeader><CardTitle className="text-sm">Hub Settings</CardTitle></CardHeader>
            <CardContent className="space-y-2 text-sm">
              <div className="flex justify-between">
                <span className="text-muted-foreground">Remote</span>
                <span className="font-mono text-xs">{cfg.remote}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Stale Lock Timeout</span>
                <span className="font-mono text-xs">{cfg.stale_lock_timeout_minutes}m</span>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader><CardTitle className="text-sm">Tracking Mode</CardTitle></CardHeader>
            <CardContent className="space-y-3">
              <div className="flex gap-2">
                {(["strict", "normal", "relaxed"] as const).map((mode) => (
                  <button
                    key={mode}
                    onClick={() => setTrackingMode(mode)}
                    className="focus:outline-none"
                  >
                    <Badge
                      variant={trackingMode === mode ? "default" : "outline"}
                      className="cursor-pointer capitalize"
                    >
                      {mode}
                    </Badge>
                  </button>
                ))}
              </div>
              <p className="text-xs text-muted-foreground">
                {trackingMode === "strict"
                  ? "All code changes require an active crosslink issue."
                  : trackingMode === "normal"
                    ? "Issue required for commits, relaxed for reads."
                    : "No enforcement — development mode."}
              </p>
              <Button size="sm" onClick={handleSave} disabled={saving}>
                Save
              </Button>
            </CardContent>
          </Card>
        </>
      )}
    </div>
  );
}
