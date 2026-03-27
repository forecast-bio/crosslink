import { useEffect, useState } from "react";
import { config as configApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Settings, Save, RotateCcw, Shield, Clock, GitBranch, Eye } from "lucide-react";
import type { Config as ConfigType, TrackingMode, SigningEnforcement } from "@/lib/types";

export function Config() {
  const [cfg, setCfg] = useState<ConfigType | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveResult, setSaveResult] = useState<"ok" | "error" | null>(null);

  // Editable form state (mirrors ConfigResponse fields)
  const [trackingMode, setTrackingMode] = useState<TrackingMode>("normal");
  const [staleLockTimeout, setStaleLockTimeout] = useState(60);
  const [remote, setRemote] = useState("origin");
  const [signingEnforcement, setSigningEnforcement] = useState<SigningEnforcement>("disabled");
  const [interventionTracking, setInterventionTracking] = useState(true);
  const [autoStealStaleLocks, setAutoStealStaleLocks] = useState(false);

  const loadConfig = () => {
    setLoading(true);
    configApi
      .get()
      .then((c) => {
        setCfg(c);
        setTrackingMode(c.tracking_mode);
        setStaleLockTimeout(c.stale_lock_timeout_minutes);
        setRemote(c.remote);
        setSigningEnforcement(c.signing_enforcement);
        setInterventionTracking(c.intervention_tracking);
        setAutoStealStaleLocks(c.auto_steal_stale_locks);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  };

  useEffect(() => { loadConfig(); }, []);

  const isDirty =
    cfg !== null &&
    (trackingMode !== cfg.tracking_mode ||
      staleLockTimeout !== cfg.stale_lock_timeout_minutes ||
      remote !== cfg.remote ||
      signingEnforcement !== cfg.signing_enforcement ||
      interventionTracking !== cfg.intervention_tracking ||
      autoStealStaleLocks !== cfg.auto_steal_stale_locks);

  const handleSave = async () => {
    setSaving(true);
    setSaveResult(null);
    const updated = await configApi
      .update({
        tracking_mode: trackingMode,
        stale_lock_timeout_minutes: staleLockTimeout,
        remote,
        signing_enforcement: signingEnforcement,
        intervention_tracking: interventionTracking,
        auto_steal_stale_locks: autoStealStaleLocks,
      })
      .catch(() => null);
    if (updated) {
      setCfg(updated);
      setSaveResult("ok");
    } else {
      setSaveResult("error");
    }
    setSaving(false);
    setTimeout(() => setSaveResult(null), 3000);
  };

  const handleReset = () => {
    if (!cfg) return;
    setTrackingMode(cfg.tracking_mode);
    setStaleLockTimeout(cfg.stale_lock_timeout_minutes);
    setRemote(cfg.remote);
    setSigningEnforcement(cfg.signing_enforcement);
    setInterventionTracking(cfg.intervention_tracking);
    setAutoStealStaleLocks(cfg.auto_steal_stale_locks);
  };

  if (loading) {
    return (
      <div className="p-6">
        <h1 className="text-2xl font-bold mb-4">Config</h1>
        <p className="text-muted-foreground text-sm">Loading…</p>
      </div>
    );
  }

  if (error || !cfg) {
    return (
      <div className="p-6">
        <h1 className="text-2xl font-bold mb-4">Config</h1>
        <p className={error ? "text-destructive text-sm" : "text-muted-foreground text-sm"}>
          {error ?? "Server unavailable."}
        </p>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 max-w-2xl">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold flex items-center gap-2">
          <Settings className="h-6 w-6" />
          Config
        </h1>
        <div className="flex items-center gap-2">
          {saveResult === "ok" && (
            <Badge variant="success">Saved</Badge>
          )}
          {saveResult === "error" && (
            <Badge variant="destructive">Save failed</Badge>
          )}
          <Button
            variant="ghost"
            size="sm"
            onClick={handleReset}
            disabled={!isDirty || saving}
          >
            <RotateCcw className="h-4 w-4 mr-1" />
            Reset
          </Button>
          <Button
            size="sm"
            onClick={handleSave}
            disabled={!isDirty || saving}
          >
            <Save className="h-4 w-4 mr-1" />
            {saving ? "Saving…" : "Save"}
          </Button>
        </div>
      </div>

      <p className="text-sm text-muted-foreground">
        Edit the crosslink hook configuration. Changes are written to{" "}
        <code className="text-xs bg-muted px-1 py-0.5 rounded">.crosslink/hook-config.json</code>.
      </p>

      {/* Tracking Mode */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm flex items-center gap-2">
            <Eye className="h-4 w-4" />
            Tracking Mode
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="flex gap-2">
            {(["strict", "normal", "relaxed"] as const).map((mode) => (
              <button
                key={mode}
                onClick={() => setTrackingMode(mode)}
                className="focus:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-md"
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
          <p className="text-xs text-muted-foreground leading-relaxed">
            {trackingMode === "strict"
              ? "All code changes require an active crosslink issue. Commits and edits are blocked without one."
              : trackingMode === "normal"
                ? "Issue required for commits. Read operations and exploration are unrestricted."
                : "No enforcement — development mode. Useful for initial setup or experimentation."}
          </p>
        </CardContent>
      </Card>

      {/* Hub Settings */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm flex items-center gap-2">
            <GitBranch className="h-4 w-4" />
            Hub Settings
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <FieldRow label="Remote" description="Git remote used for the hub branch.">
            <Input
              value={remote}
              onChange={(e) => setRemote(e.target.value)}
              className="w-48 h-8 text-sm font-mono"
              placeholder="origin"
            />
          </FieldRow>
          <Separator />
          <FieldRow
            label="Stale Lock Timeout"
            description="Minutes before a lock is considered stale (agent presumed dead)."
          >
            <div className="flex items-center gap-2">
              <Input
                type="number"
                min={1}
                max={1440}
                value={staleLockTimeout}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  if (!isNaN(v) && v >= 1) setStaleLockTimeout(v);
                }}
                className="w-24 h-8 text-sm font-mono"
              />
              <span className="text-xs text-muted-foreground">minutes</span>
            </div>
          </FieldRow>
        </CardContent>
      </Card>

      {/* Security */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm flex items-center gap-2">
            <Shield className="h-4 w-4" />
            Security
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <FieldRow
            label="Signing Enforcement"
            description="Controls whether commits and lock operations require valid SSH signatures."
          >
            <div className="flex gap-2">
              {(["disabled", "audit", "required"] as const).map((mode) => (
                <button
                  key={mode}
                  onClick={() => setSigningEnforcement(mode)}
                  className="focus:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-md"
                >
                  <Badge
                    variant={signingEnforcement === mode ? "default" : "outline"}
                    className="cursor-pointer capitalize"
                  >
                    {mode}
                  </Badge>
                </button>
              ))}
            </div>
          </FieldRow>
          <p className="text-xs text-muted-foreground leading-relaxed">
            {signingEnforcement === "disabled"
              ? "Signatures are not checked. Suitable for local-only development."
              : signingEnforcement === "audit"
                ? "Signatures are verified and logged but unsigned operations are allowed."
                : "All lock claims and releases must be signed. Unsigned operations are rejected."}
          </p>
        </CardContent>
      </Card>

      {/* Behavior */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm flex items-center gap-2">
            <Clock className="h-4 w-4" />
            Behavior
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <ToggleRow
            label="Intervention Tracking"
            description="Log when a human driver intervenes in agent work (tool rejections, redirects, manual actions)."
            checked={interventionTracking}
            onChange={setInterventionTracking}
          />
          <Separator />
          <ToggleRow
            label="Auto-Steal Stale Locks"
            description="Automatically reclaim locks held by agents that have gone stale. When disabled, stale locks must be manually released."
            checked={autoStealStaleLocks}
            onChange={setAutoStealStaleLocks}
          />
        </CardContent>
      </Card>

      {/* Sticky save bar when dirty */}
      {isDirty && (
        <div className="sticky bottom-4 flex justify-end">
          <Card className="shadow-lg border-primary/20">
            <CardContent className="p-3 flex items-center gap-3">
              <span className="text-sm text-muted-foreground">Unsaved changes</span>
              <Button variant="ghost" size="sm" onClick={handleReset} disabled={saving}>
                Discard
              </Button>
              <Button size="sm" onClick={handleSave} disabled={saving}>
                <Save className="h-4 w-4 mr-1" />
                {saving ? "Saving…" : "Save changes"}
              </Button>
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Reusable layout helpers
// ---------------------------------------------------------------------------

function FieldRow({
  label,
  description,
  children,
}: {
  label: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium">{label}</p>
        <p className="text-xs text-muted-foreground mt-0.5 leading-relaxed">{description}</p>
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

function ToggleRow({
  label,
  description,
  checked,
  onChange,
}: {
  label: string;
  description: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium">{label}</p>
        <p className="text-xs text-muted-foreground mt-0.5 leading-relaxed">{description}</p>
      </div>
      <button
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={[
          "relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full border-2 border-transparent transition-colors",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
          checked ? "bg-primary" : "bg-muted",
        ].join(" ")}
      >
        <span
          className={[
            "pointer-events-none inline-block h-4 w-4 rounded-full bg-background shadow-sm ring-0 transition-transform",
            checked ? "translate-x-4" : "translate-x-0",
          ].join(" ")}
        />
      </button>
    </div>
  );
}
