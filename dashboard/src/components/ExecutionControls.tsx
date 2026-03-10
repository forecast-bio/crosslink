import { useOrchestratorStore } from "@/stores/orchestrator";
import { orchestrator as orchestratorApi } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Play, Pause, RotateCcw } from "lucide-react";
import type { ExecutionState } from "@/lib/types";

const stateVariant: Record<string, "success" | "warning" | "destructive" | "info" | "secondary"> = {
  running: "info",
  paused: "warning",
  done: "success",
  failed: "destructive",
  idle: "secondary",
};

const stateLabel: Record<string, string> = {
  running: "Running",
  paused: "Paused",
  done: "Completed",
  failed: "Failed",
  idle: "Idle",
};

export function ExecutionControls() {
  const { plan, executionStatus, progressPct, fetchStatus } = useOrchestratorStore();
  const state = executionStatus as ExecutionState;

  const handlePause = async () => {
    await orchestratorApi.pause().catch(() => {});
    void fetchStatus();
  };

  const handleResume = async () => {
    await orchestratorApi.execute().catch(() => {});
    void fetchStatus();
  };

  const handleStart = async () => {
    await orchestratorApi.execute().catch(() => {});
    void fetchStatus();
  };

  // Compute phase progress
  const phases = plan?.phases ?? [];
  const phaseProgress = phases.map((phase) => {
    const total = phase.stages.length;
    const done = phase.stages.filter(
      (s) => s.status === "done" || s.status === "skipped",
    ).length;
    const running = phase.stages.filter((s) => s.status === "running").length;
    const failed = phase.stages.filter((s) => s.status === "failed").length;
    return { id: phase.id, title: phase.title, total, done, running, failed };
  });

  return (
    <div className="space-y-4">
      {/* Control bar */}
      <div className="flex items-center justify-between rounded-lg border bg-card p-4">
        <div className="flex items-center gap-3">
          <Badge variant={stateVariant[state] ?? "secondary"}>
            {stateLabel[state] ?? state}
          </Badge>
          {state !== "idle" && (
            <div className="flex items-center gap-2">
              <div className="h-2 w-32 rounded-full bg-secondary overflow-hidden">
                <div
                  className="h-full rounded-full bg-blue-500 transition-all duration-500"
                  style={{ width: `${progressPct}%` }}
                />
              </div>
              <span className="text-sm font-mono text-muted-foreground">
                {progressPct}%
              </span>
            </div>
          )}
        </div>

        <div className="flex items-center gap-2">
          {state === "idle" && plan && (
            <Button size="sm" onClick={handleStart}>
              <Play className="h-4 w-4 mr-1" /> Start
            </Button>
          )}
          {state === "running" && (
            <Button size="sm" variant="outline" onClick={handlePause}>
              <Pause className="h-4 w-4 mr-1" /> Pause
            </Button>
          )}
          {state === "paused" && (
            <Button size="sm" onClick={handleResume}>
              <Play className="h-4 w-4 mr-1" /> Resume
            </Button>
          )}
          {state === "failed" && (
            <Button size="sm" onClick={handleStart}>
              <RotateCcw className="h-4 w-4 mr-1" /> Restart
            </Button>
          )}
        </div>
      </div>

      {/* Phase progress indicators */}
      {phaseProgress.length > 0 && state !== "idle" && (
        <div className="grid gap-2">
          {phaseProgress.map((phase) => {
            const pct =
              phase.total > 0
                ? Math.round((phase.done / phase.total) * 100)
                : 0;
            const isActive = phase.running > 0;
            const hasFailed = phase.failed > 0;

            return (
              <div
                key={phase.id}
                className="flex items-center gap-3 rounded-md bg-muted/30 px-3 py-2"
              >
                <div className="flex-1 min-w-0">
                  <div className="flex items-center justify-between mb-1">
                    <span className="text-xs font-medium truncate">
                      {phase.title}
                    </span>
                    <span className="text-xs text-muted-foreground ml-2 shrink-0">
                      {phase.done}/{phase.total}
                    </span>
                  </div>
                  <div className="h-1.5 w-full rounded-full bg-secondary overflow-hidden">
                    <div
                      className={`h-full rounded-full transition-all duration-500 ${
                        hasFailed
                          ? "bg-red-500"
                          : isActive
                            ? "bg-blue-500"
                            : pct === 100
                              ? "bg-green-500"
                              : "bg-blue-500/60"
                      }`}
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                </div>
                {isActive && (
                  <span className="relative flex h-2 w-2 shrink-0">
                    <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-blue-400 opacity-75" />
                    <span className="relative inline-flex rounded-full h-2 w-2 bg-blue-500" />
                  </span>
                )}
                {hasFailed && !isActive && (
                  <span className="h-2 w-2 rounded-full bg-red-500 shrink-0" />
                )}
                {pct === 100 && !hasFailed && (
                  <span className="h-2 w-2 rounded-full bg-green-500 shrink-0" />
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
