import { useEffect } from "react";
import { useOrchestratorStore } from "@/stores/orchestrator";
import { orchestrator as orchestratorApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Pause, Play } from "lucide-react";

export function Execution() {
  const { plan, executionStatus, progressPct, fetchPlan, fetchStatus } = useOrchestratorStore();

  useEffect(() => {
    void fetchPlan();
    void fetchStatus();
    const id = setInterval(() => { void fetchStatus(); }, 5000);
    return () => clearInterval(id);
  }, [fetchPlan, fetchStatus]);

  const handlePause = async () => {
    await orchestratorApi.pause().catch(() => {});
    void fetchStatus();
  };

  const handleResume = async () => {
    await orchestratorApi.execute().catch(() => {});
    void fetchStatus();
  };

  return (
    <div className="p-6 space-y-4 max-w-3xl">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Execution</h1>
        <div className="flex items-center gap-2">
          <Badge variant={
            executionStatus === "running" ? "success" :
            executionStatus === "paused" ? "warning" :
            executionStatus === "failed" ? "destructive" :
            "secondary"
          }>
            {executionStatus}
          </Badge>
          {executionStatus === "running" ? (
            <Button size="sm" variant="outline" onClick={handlePause}>
              <Pause className="h-4 w-4 mr-1" /> Pause
            </Button>
          ) : executionStatus === "paused" ? (
            <Button size="sm" onClick={handleResume}>
              <Play className="h-4 w-4 mr-1" /> Resume
            </Button>
          ) : null}
        </div>
      </div>

      {executionStatus !== "idle" && (
        <Card>
          <CardHeader><CardTitle className="text-sm">Overall Progress</CardTitle></CardHeader>
          <CardContent className="space-y-2">
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Progress</span>
              <span>{progressPct}%</span>
            </div>
            <div className="h-2 w-full rounded-full bg-secondary overflow-hidden">
              <div
                className="h-full rounded-full bg-blue-500 transition-all duration-500"
                style={{ width: `${progressPct}%` }}
              />
            </div>
          </CardContent>
        </Card>
      )}

      {plan ? (
        <div className="space-y-3">
          {plan.phases.map((phase) => (
            <Card key={phase.id}>
              <CardHeader className="pb-2">
                <CardTitle className="text-sm">{phase.title}</CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                {phase.stages.map((stage) => (
                  <div key={stage.id} className="flex items-center justify-between rounded-md bg-muted/30 px-3 py-2 text-sm">
                    <div>
                      <p className="font-medium">{stage.title}</p>
                      {stage.agent_id && (
                        <p className="text-xs text-muted-foreground font-mono">{stage.agent_id}</p>
                      )}
                    </div>
                    <Badge variant={
                      stage.status === "done" ? "success" :
                      stage.status === "running" ? "info" :
                      stage.status === "blocked" ? "warning" :
                      stage.status === "failed" ? "destructive" :
                      "secondary"
                    }>
                      {stage.status}
                    </Badge>
                  </div>
                ))}
              </CardContent>
            </Card>
          ))}
        </div>
      ) : (
        <Card>
          <CardContent className="py-10 text-center text-muted-foreground text-sm">
            No execution plan. Go to Orchestrator to import and decompose a design document.
          </CardContent>
        </Card>
      )}
    </div>
  );
}
