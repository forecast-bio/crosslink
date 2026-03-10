import { useCallback, useEffect, useState } from "react";
import { useOrchestratorStore } from "@/stores/orchestrator";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { DagGraph } from "@/components/DagGraph";
import { GanttChart } from "@/components/GanttChart";
import { ExecutionControls } from "@/components/ExecutionControls";
import { StageDetail } from "@/components/StageDetail";
import { AgentLogStream } from "@/components/AgentLogStream";
import { GitBranch, BarChart3 } from "lucide-react";
import type { OrchestratorPlan } from "@/lib/types";

type ViewMode = "dag" | "gantt";

/** Compute summary counts from a plan. */
function summarize(plan: OrchestratorPlan) {
  let done = 0;
  let running = 0;
  let failed = 0;
  let total = 0;
  for (const phase of plan.phases) {
    for (const stage of phase.stages) {
      total++;
      if (stage.status === "done") done++;
      else if (stage.status === "running") running++;
      else if (stage.status === "failed") failed++;
    }
  }
  return { done, running, failed, total };
}

export function Execution() {
  const { plan, executionStatus, progressPct, fetchPlan, fetchStatus, selectStage, selectedStageId } =
    useOrchestratorStore();

  const [viewMode, setViewMode] = useState<ViewMode>("dag");

  useEffect(() => {
    void fetchPlan();
    void fetchStatus();
    const id = setInterval(() => {
      void fetchStatus();
      void fetchPlan();
    }, 10_000);
    return () => clearInterval(id);
  }, [fetchPlan, fetchStatus]);

  const handleStageClick = useCallback((stageId: string) => {
    selectStage(selectedStageId === stageId ? null : stageId);
  }, [selectStage, selectedStageId]);

  const stats = plan ? summarize(plan) : null;

  return (
    <div className="p-6 space-y-4">
      {/* Header with execution controls */}
      <div className="flex items-center justify-between flex-wrap gap-2">
        <h1 className="text-2xl font-bold">Execution</h1>
        <ExecutionControls />
      </div>

      {/* Progress + summary stats */}
      {executionStatus !== "idle" && stats && (
        <Card>
          <CardContent className="pt-4 space-y-3">
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Overall Progress</span>
              <span className="font-medium">{progressPct}%</span>
            </div>
            <div className="h-2 w-full rounded-full bg-secondary overflow-hidden">
              <div
                className="h-full rounded-full bg-blue-500 transition-all duration-500"
                style={{ width: `${progressPct}%` }}
              />
            </div>
            <div className="flex gap-4 text-xs text-muted-foreground">
              <span>{stats.done}/{stats.total} done</span>
              {stats.running > 0 && (
                <span className="text-blue-400">{stats.running} running</span>
              )}
              {stats.failed > 0 && (
                <span className="text-red-400">{stats.failed} failed</span>
              )}
            </div>
          </CardContent>
        </Card>
      )}

      {plan ? (
        <>
          {/* View mode toggle */}
          <div className="flex items-center gap-1 rounded-lg border border-border p-1 w-fit">
            <Button
              size="sm"
              variant={viewMode === "dag" ? "default" : "ghost"}
              className="h-7 px-3 text-xs"
              onClick={() => setViewMode("dag")}
            >
              <GitBranch className="h-3.5 w-3.5 mr-1" /> DAG
            </Button>
            <Button
              size="sm"
              variant={viewMode === "gantt" ? "default" : "ghost"}
              className="h-7 px-3 text-xs"
              onClick={() => setViewMode("gantt")}
            >
              <BarChart3 className="h-3.5 w-3.5 mr-1" /> Gantt
            </Button>
          </div>

          {/* Visualization + optional detail panel */}
          <div className="flex gap-4">
            <div className="flex-1 min-w-0 space-y-4">
              {viewMode === "dag" ? (
                <DagGraph plan={plan} onStageClick={handleStageClick} />
              ) : (
                <GanttChart plan={plan} onStageClick={handleStageClick} />
              )}

              {/* Event log */}
              {executionStatus !== "idle" && <AgentLogStream />}
            </div>

            {selectedStageId && <StageDetail />}
          </div>
        </>
      ) : (
        <Card>
          <CardContent className="py-10 text-center text-muted-foreground text-sm">
            No execution plan. Go to Orchestrator to import and decompose a
            design document.
          </CardContent>
        </Card>
      )}
    </div>
  );
}
