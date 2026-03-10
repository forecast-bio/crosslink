import { useEffect } from "react";
import { useOrchestratorStore } from "@/stores/orchestrator";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { ExecutionControls } from "@/components/ExecutionControls";
import { StageDetail } from "@/components/StageDetail";
import { AgentLogStream } from "@/components/AgentLogStream";

export function Execution() {
  const { plan, executionStatus, fetchPlan, fetchStatus, selectStage, selectedStageId } =
    useOrchestratorStore();

  useEffect(() => {
    void fetchPlan();
    void fetchStatus();
    const id = setInterval(() => {
      void fetchStatus();
    }, 5000);
    return () => clearInterval(id);
  }, [fetchPlan, fetchStatus]);

  return (
    <div className="p-6 space-y-4">
      <h1 className="text-2xl font-bold">Execution</h1>

      <ExecutionControls />

      {plan ? (
        <div className="flex gap-4">
          {/* Main content: phase/stage list + event log */}
          <div className="flex-1 min-w-0 space-y-4">
            {/* Phase/stage breakdown */}
            <div className="space-y-3">
              {plan.phases.map((phase) => (
                <Card key={phase.id}>
                  <CardHeader className="pb-2">
                    <CardTitle className="text-sm">{phase.title}</CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-2">
                    {phase.stages.map((stage) => {
                      const isSelected = stage.id === selectedStageId;
                      return (
                        <div
                          key={stage.id}
                          className={`flex items-center justify-between rounded-md px-3 py-2 text-sm cursor-pointer transition-colors ${
                            isSelected
                              ? "bg-accent ring-1 ring-accent-foreground/20"
                              : "bg-muted/30 hover:bg-muted/50"
                          }`}
                          onClick={() =>
                            selectStage(isSelected ? null : stage.id)
                          }
                        >
                          <div className="min-w-0">
                            <p className="font-medium truncate">{stage.title}</p>
                            {stage.agent_id && (
                              <p className="text-xs text-muted-foreground font-mono truncate">
                                {stage.agent_id}
                              </p>
                            )}
                          </div>
                          <Badge
                            variant={
                              stage.status === "done"
                                ? "success"
                                : stage.status === "running"
                                  ? "info"
                                  : stage.status === "blocked"
                                    ? "warning"
                                    : stage.status === "failed"
                                      ? "destructive"
                                      : "secondary"
                            }
                          >
                            {stage.status ?? "pending"}
                          </Badge>
                        </div>
                      );
                    })}
                  </CardContent>
                </Card>
              ))}
            </div>

            {/* Event log */}
            {executionStatus !== "idle" && <AgentLogStream />}
          </div>

          {/* Stage detail sidebar */}
          {selectedStageId && <StageDetail />}
        </div>
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
