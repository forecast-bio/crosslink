import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { useOrchestratorStore } from "@/stores/orchestrator";
import { agents as agentsApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import {
  ExternalLink,
  User,
  Activity,
  FileText,
  X,
  RotateCcw,
  SkipForward,
} from "lucide-react";
import type { AgentDetailResponse, StageStatus } from "@/lib/types";
import { formatRelativeTime } from "@/lib/utils";

const statusVariant: Record<string, "success" | "warning" | "destructive" | "info" | "secondary"> = {
  running: "info",
  done: "success",
  failed: "destructive",
  blocked: "warning",
  skipped: "secondary",
  pending: "secondary",
};

export function StageDetail() {
  const navigate = useNavigate();
  const { plan, selectedStageId, selectStage, retryStage, skipStage } =
    useOrchestratorStore();
  const [agentDetail, setAgentDetail] = useState<AgentDetailResponse | null>(null);
  const [loadingAgent, setLoadingAgent] = useState(false);

  // Find the selected stage and its parent phase
  let selectedStage = null;
  let parentPhase = null;
  if (plan && selectedStageId) {
    for (const phase of plan.phases) {
      const stage = phase.stages.find((s) => s.id === selectedStageId);
      if (stage) {
        selectedStage = stage;
        parentPhase = phase;
        break;
      }
    }
  }

  const agentId = selectedStage?.agent_id ?? null;
  const stageStatus = selectedStage?.status ?? "pending";

  // Fetch agent details when stage has an assigned agent
  useEffect(() => {
    if (!agentId) {
      setAgentDetail(null);
      return;
    }
    let cancelled = false;
    setLoadingAgent(true);
    agentsApi
      .get(agentId)
      .then((data) => {
        if (!cancelled) setAgentDetail(data);
      })
      .catch(() => {
        if (!cancelled) setAgentDetail(null);
      })
      .finally(() => {
        if (!cancelled) setLoadingAgent(false);
      });
    return () => {
      cancelled = true;
    };
  }, [agentId]);

  // Auto-refresh agent detail for running stages
  useEffect(() => {
    if (stageStatus !== "running" || !agentId) return;
    const id = setInterval(() => {
      agentsApi
        .get(agentId)
        .then(setAgentDetail)
        .catch(() => {});
    }, 10_000);
    return () => clearInterval(id);
  }, [stageStatus, agentId]);

  if (!selectedStage || !parentPhase) return null;

  const status = stageStatus as StageStatus;
  const isRunning = status === "running";
  const isFailed = status === "failed";

  return (
    <Card className="w-80 shrink-0">
      <CardHeader className="pb-3">
        <div className="flex items-start justify-between">
          <div className="space-y-1 min-w-0 flex-1">
            <CardTitle className="text-sm leading-tight">
              {selectedStage.title}
            </CardTitle>
            <p className="text-xs text-muted-foreground">{parentPhase.title}</p>
          </div>
          <Button
            variant="ghost"
            size="sm"
            className="h-6 w-6 p-0 shrink-0"
            onClick={() => selectStage(null)}
          >
            <X className="h-4 w-4" />
          </Button>
        </div>
      </CardHeader>

      <CardContent className="space-y-4">
        {/* Status */}
        <div className="flex items-center gap-2">
          <Badge variant={statusVariant[status] ?? "secondary"}>
            {status}
          </Badge>
          {isRunning && (
            <span className="relative flex h-2 w-2">
              <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-blue-400 opacity-75" />
              <span className="relative inline-flex rounded-full h-2 w-2 bg-blue-500" />
            </span>
          )}
        </div>

        {/* Description */}
        {selectedStage.description && (
          <p className="text-xs text-muted-foreground leading-relaxed">
            {selectedStage.description}
          </p>
        )}

        {/* Complexity & dependencies */}
        <div className="grid grid-cols-2 gap-2 text-xs">
          <div>
            <span className="text-muted-foreground">Complexity</span>
            <p className="font-medium">{selectedStage.complexity_hours}h est.</p>
          </div>
          <div>
            <span className="text-muted-foreground">Dependencies</span>
            <p className="font-medium">
              {selectedStage.depends_on.length > 0
                ? selectedStage.depends_on.length
                : "None"}
            </p>
          </div>
        </div>

        <Separator />

        {/* Assigned agent */}
        <div className="space-y-2">
          <div className="flex items-center gap-1.5 text-xs font-medium">
            <User className="h-3.5 w-3.5" />
            Agent
          </div>
          {selectedStage.agent_id ? (
            <div className="space-y-2">
              <p className="text-xs font-mono text-muted-foreground break-all">
                {selectedStage.agent_id}
              </p>

              {/* Live heartbeat indicator */}
              {loadingAgent ? (
                <p className="text-xs text-muted-foreground">Loading agent info...</p>
              ) : agentDetail ? (
                <div className="space-y-1.5">
                  <div className="flex items-center gap-1.5 text-xs">
                    <Activity className="h-3.5 w-3.5" />
                    <span className="text-muted-foreground">Heartbeat:</span>
                    {agentDetail.last_heartbeat ? (
                      <span
                        className={
                          isRunning ? "text-green-400" : "text-muted-foreground"
                        }
                      >
                        {formatRelativeTime(agentDetail.last_heartbeat.timestamp)}
                      </span>
                    ) : (
                      <span className="text-muted-foreground">No heartbeat</span>
                    )}
                  </div>

                  {agentDetail.branch && (
                    <p className="text-xs text-muted-foreground truncate">
                      Branch: <span className="font-mono">{agentDetail.branch}</span>
                    </p>
                  )}

                  {/* Kickoff report */}
                  {agentDetail.kickoff_report && (
                    <div className="space-y-1">
                      <div className="flex items-center gap-1.5 text-xs font-medium">
                        <FileText className="h-3.5 w-3.5" />
                        Kickoff Report
                      </div>
                      <p className="text-xs text-muted-foreground whitespace-pre-wrap max-h-24 overflow-y-auto rounded bg-muted/50 p-2">
                        {agentDetail.kickoff_report}
                      </p>
                    </div>
                  )}

                  {agentDetail.kickoff_status && !agentDetail.kickoff_report && (
                    <p className="text-xs text-muted-foreground">
                      Status: {agentDetail.kickoff_status}
                    </p>
                  )}
                </div>
              ) : null}

              {/* View agent button */}
              <Button
                size="sm"
                variant="outline"
                className="w-full text-xs"
                onClick={() =>
                  navigate(
                    `/agents/${encodeURIComponent(selectedStage!.agent_id!)}`,
                  )
                }
              >
                <ExternalLink className="h-3.5 w-3.5 mr-1" />
                View Agent
              </Button>
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">No agent assigned</p>
          )}
        </div>

        {/* Tasks */}
        {selectedStage.tasks.length > 0 && (
          <>
            <Separator />
            <div className="space-y-1.5">
              <span className="text-xs font-medium">
                Tasks ({selectedStage.tasks.length})
              </span>
              {selectedStage.tasks.map((task) => (
                <div
                  key={task.id}
                  className="text-xs text-muted-foreground pl-2 border-l border-border"
                >
                  {task.title}
                </div>
              ))}
            </div>
          </>
        )}

        {/* Failure actions */}
        {isFailed && (
          <>
            <Separator />
            <div className="flex gap-2">
              <Button
                size="sm"
                variant="outline"
                className="flex-1 text-xs"
                onClick={() => retryStage(selectedStage!.id).catch(() => {})}
              >
                <RotateCcw className="h-3.5 w-3.5 mr-1" />
                Retry
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="flex-1 text-xs"
                onClick={() => skipStage(selectedStage!.id).catch(() => {})}
              >
                <SkipForward className="h-3.5 w-3.5 mr-1" />
                Skip
              </Button>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}
