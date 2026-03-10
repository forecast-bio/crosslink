import { useEffect } from "react";
import { Link } from "react-router";
import { orchestrator as orchestratorApi } from "@/api/client";
import { useOrchestratorStore } from "@/stores/orchestrator";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { DocumentImport } from "@/components/DocumentImport";
import { StageEditor } from "@/components/StageEditor";
import { Play, RotateCcw, GitFork } from "lucide-react";

export function Orchestrator() {
  const {
    plan,
    loading,
    decomposing,
    error,
    executionStatus,
    fetchPlan,
    fetchStatus,
    decompose,
    clearPlan,
    updatePhase,
    addStage,
    removeStage,
    updateStage,
    addDependency,
    removeDependency,
    reorderStages,
  } = useOrchestratorStore();

  useEffect(() => {
    void fetchPlan();
    void fetchStatus();
  }, [fetchPlan, fetchStatus]);

  const handleDecompose = async (document: string) => {
    await decompose(document);
  };

  const handleExecute = async () => {
    await orchestratorApi.execute().catch(() => {});
    await fetchStatus();
  };

  const isRunning = executionStatus === "running";
  const isEditable = !isRunning && executionStatus !== "paused";

  return (
    <div className="p-6 space-y-4 max-w-4xl">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Orchestrator</h1>
        <div className="flex items-center gap-2">
          {executionStatus !== "idle" && (
            <Badge
              variant={
                isRunning ? "success" :
                executionStatus === "paused" ? "warning" :
                executionStatus === "failed" ? "destructive" :
                executionStatus === "done" ? "success" :
                "secondary"
              }
            >
              {executionStatus}
            </Badge>
          )}
          {isRunning && (
            <Link to="/execution">
              <Button size="sm" variant="outline">
                <GitFork className="h-4 w-4 mr-1" />
                View Execution
              </Button>
            </Link>
          )}
          {plan && isEditable && (
            <>
              <Button size="sm" variant="ghost" onClick={clearPlan}>
                <RotateCcw className="h-4 w-4 mr-1" />
                Reset
              </Button>
              <Button size="sm" onClick={() => void handleExecute()}>
                <Play className="h-4 w-4 mr-1" />
                Execute Plan
              </Button>
            </>
          )}
        </div>
      </div>

      {/* Error display */}
      {error && (
        <Card className="border-destructive">
          <CardContent className="py-3 text-sm text-destructive">
            {error}
          </CardContent>
        </Card>
      )}

      {/* Import section — shown when no plan loaded */}
      {!plan && !loading && (
        <DocumentImport onDecompose={handleDecompose} decomposing={decomposing} />
      )}

      {/* Loading state */}
      {loading && (
        <p className="text-muted-foreground text-sm">Loading plan...</p>
      )}

      {/* Plan editor */}
      {plan && (
        <StageEditor
          plan={plan}
          onUpdatePhase={updatePhase}
          onAddStage={addStage}
          onRemoveStage={removeStage}
          onUpdateStage={updateStage}
          onAddDependency={addDependency}
          onRemoveDependency={removeDependency}
          onReorderStages={reorderStages}
          readOnly={!isEditable}
        />
      )}

      {/* Empty state when plan loaded but no phases */}
      {plan && plan.phases.length === 0 && (
        <Card>
          <CardContent className="py-10 text-center text-muted-foreground text-sm">
            The plan has no phases. Try importing a more detailed design document.
          </CardContent>
        </Card>
      )}
    </div>
  );
}
