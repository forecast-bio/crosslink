import { useEffect, useState } from "react";
import { orchestrator as orchestratorApi } from "@/api/client";
import { useOrchestratorStore } from "@/stores/orchestrator";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Layers, Play } from "lucide-react";

export function Orchestrator() {
  const { plan, loading, fetchPlan, executionStatus, fetchStatus } = useOrchestratorStore();
  const [docText, setDocText] = useState("");
  const [decomposing, setDecomposing] = useState(false);
  const [executing, setExecuting] = useState(false);

  useEffect(() => {
    void fetchPlan();
    void fetchStatus();
  }, [fetchPlan, fetchStatus]);

  const handleDecompose = async () => {
    if (!docText.trim()) return;
    setDecomposing(true);
    const result = await orchestratorApi.decompose(docText).catch(() => null);
    if (result) useOrchestratorStore.getState().setPlan(result);
    setDecomposing(false);
  };

  const handleExecute = async () => {
    setExecuting(true);
    await orchestratorApi.execute().catch(() => {});
    await fetchStatus();
    setExecuting(false);
  };

  return (
    <div className="p-6 space-y-4 max-w-3xl">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Orchestrator</h1>
        {plan && (
          <Button size="sm" onClick={handleExecute} disabled={executing || executionStatus === "running"}>
            <Play className="h-4 w-4 mr-1" />
            {executionStatus === "running" ? "Running…" : "Execute Plan"}
          </Button>
        )}
      </div>

      <Card>
        <CardHeader><CardTitle className="text-sm">Import Design Document</CardTitle></CardHeader>
        <CardContent className="space-y-3">
          <textarea
            className="w-full h-40 rounded-md border border-input bg-background px-3 py-2 text-sm font-mono resize-y focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring placeholder:text-muted-foreground"
            placeholder="Paste your design document (Markdown) here…"
            value={docText}
            onChange={(e) => setDocText(e.target.value)}
          />
          <Button size="sm" onClick={handleDecompose} disabled={decomposing || !docText.trim()}>
            <Layers className="h-4 w-4 mr-1" />
            {decomposing ? "Decomposing…" : "Decompose"}
          </Button>
        </CardContent>
      </Card>

      {loading ? (
        <p className="text-muted-foreground text-sm">Loading plan…</p>
      ) : plan ? (
        <div className="space-y-3">
          <h2 className="text-lg font-semibold">{plan.title}</h2>
          {plan.phases.map((phase) => (
            <Card key={phase.id}>
              <CardHeader className="pb-2">
                <CardTitle className="text-sm">{phase.title}</CardTitle>
                {phase.description && (
                  <p className="text-xs text-muted-foreground">{phase.description}</p>
                )}
              </CardHeader>
              <CardContent className="space-y-2">
                {phase.stages.map((stage) => (
                  <div key={stage.id} className="flex items-center justify-between rounded-md border border-border px-3 py-2 text-sm">
                    <div className="flex-1 min-w-0">
                      <p className="truncate font-medium">{stage.title}</p>
                      {stage.depends_on.length > 0 && (
                        <p className="text-xs text-muted-foreground">
                          Depends on: {stage.depends_on.join(", ")}
                        </p>
                      )}
                    </div>
                    <div className="flex items-center gap-2 shrink-0 ml-2">
                      <Badge variant={
                        stage.status === "done" ? "success" :
                        stage.status === "running" ? "info" :
                        stage.status === "failed" ? "destructive" :
                        "secondary"
                      }>
                        {stage.status}
                      </Badge>
                      <span className="text-xs text-muted-foreground">
                        ~{stage.agent_count} agent{stage.agent_count !== 1 ? "s" : ""}
                      </span>
                    </div>
                  </div>
                ))}
              </CardContent>
            </Card>
          ))}
        </div>
      ) : (
        <Card>
          <CardContent className="py-10 text-center text-muted-foreground text-sm">
            No plan loaded. Import a design document to decompose it.
          </CardContent>
        </Card>
      )}
    </div>
  );
}
