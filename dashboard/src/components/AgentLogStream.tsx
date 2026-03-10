import { useEffect, useRef } from "react";
import { useOrchestratorStore } from "@/stores/orchestrator";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Badge } from "@/components/ui/badge";
import {
  Play,
  CheckCircle2,
  XCircle,
  SkipForward,
  RotateCcw,
  Pause,
  CircleDot,
  Zap,
} from "lucide-react";
import type { ExecutionEvent, ExecutionEventKind } from "@/lib/types";

const eventIcon: Record<ExecutionEventKind, React.ReactNode> = {
  stage_started: <Play className="h-3.5 w-3.5 text-blue-400" />,
  stage_completed: <CheckCircle2 className="h-3.5 w-3.5 text-green-400" />,
  stage_failed: <XCircle className="h-3.5 w-3.5 text-red-400" />,
  stage_skipped: <SkipForward className="h-3.5 w-3.5 text-muted-foreground" />,
  stage_retried: <RotateCcw className="h-3.5 w-3.5 text-yellow-400" />,
  phase_started: <CircleDot className="h-3.5 w-3.5 text-blue-400" />,
  phase_completed: <CheckCircle2 className="h-3.5 w-3.5 text-green-400" />,
  execution_started: <Zap className="h-3.5 w-3.5 text-blue-400" />,
  execution_paused: <Pause className="h-3.5 w-3.5 text-yellow-400" />,
  execution_resumed: <Play className="h-3.5 w-3.5 text-blue-400" />,
  execution_completed: <CheckCircle2 className="h-3.5 w-3.5 text-green-500" />,
  execution_failed: <XCircle className="h-3.5 w-3.5 text-red-500" />,
};

const eventBadgeVariant: Record<string, "success" | "warning" | "destructive" | "info" | "secondary"> = {
  stage_started: "info",
  stage_completed: "success",
  stage_failed: "destructive",
  stage_skipped: "secondary",
  stage_retried: "warning",
  phase_started: "info",
  phase_completed: "success",
  execution_started: "info",
  execution_paused: "warning",
  execution_resumed: "info",
  execution_completed: "success",
  execution_failed: "destructive",
};

function formatEventTime(isoString: string): string {
  const d = new Date(isoString);
  return d.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function EventRow({
  event,
  onRetry,
  onSkip,
  onSelectStage,
}: {
  event: ExecutionEvent;
  onRetry: (stageId: string) => void;
  onSkip: (stageId: string) => void;
  onSelectStage: (stageId: string) => void;
}) {
  const isStageFailed = event.kind === "stage_failed";
  const isStageEvent = event.stage_id != null;

  return (
    <div className="flex items-start gap-2 py-1.5 px-2 hover:bg-muted/30 rounded-sm group">
      <div className="mt-0.5 shrink-0">{eventIcon[event.kind]}</div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5">
          <span className="text-xs text-muted-foreground font-mono shrink-0">
            {formatEventTime(event.timestamp)}
          </span>
          <Badge
            variant={eventBadgeVariant[event.kind] ?? "secondary"}
            className="text-[10px] px-1.5 py-0"
          >
            {event.kind.replace(/_/g, " ")}
          </Badge>
        </div>
        <p
          className={`text-xs mt-0.5 ${
            isStageEvent ? "cursor-pointer hover:underline" : ""
          }`}
          onClick={() => {
            if (isStageEvent && event.stage_id) {
              onSelectStage(event.stage_id);
            }
          }}
        >
          {event.message}
        </p>
        {event.agent_id && (
          <p className="text-[10px] text-muted-foreground font-mono mt-0.5">
            {event.agent_id}
          </p>
        )}
      </div>
      {isStageFailed && event.stage_id && (
        <div className="flex gap-1 shrink-0 opacity-0 group-hover:opacity-100 transition-opacity">
          <Button
            size="sm"
            variant="ghost"
            className="h-6 px-1.5 text-xs"
            onClick={() => onRetry(event.stage_id!)}
          >
            <RotateCcw className="h-3 w-3 mr-0.5" />
            Retry
          </Button>
          <Button
            size="sm"
            variant="ghost"
            className="h-6 px-1.5 text-xs"
            onClick={() => onSkip(event.stage_id!)}
          >
            <SkipForward className="h-3 w-3 mr-0.5" />
            Skip
          </Button>
        </div>
      )}
    </div>
  );
}

export function AgentLogStream() {
  const { events, retryStage, skipStage, selectStage } = useOrchestratorStore();
  const bottomRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom on new events
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [events.length]);

  const handleRetry = (stageId: string) => {
    retryStage(stageId).catch(() => {});
  };

  const handleSkip = (stageId: string) => {
    skipStage(stageId).catch(() => {});
  };

  return (
    <Card>
      <CardHeader className="pb-2">
        <div className="flex items-center justify-between">
          <CardTitle className="text-sm">Execution Log</CardTitle>
          {events.length > 0 && (
            <span className="text-xs text-muted-foreground">
              {events.length} event{events.length !== 1 ? "s" : ""}
            </span>
          )}
        </div>
      </CardHeader>
      <CardContent className="p-0">
        {events.length === 0 ? (
          <div className="px-4 py-8 text-center text-xs text-muted-foreground">
            No execution events yet. Start the execution to see progress here.
          </div>
        ) : (
          <ScrollArea className="h-64">
            <div className="px-2 py-1 space-y-0.5">
              {events.map((event) => (
                <EventRow
                  key={event.id}
                  event={event}
                  onRetry={handleRetry}
                  onSkip={handleSkip}
                  onSelectStage={selectStage}
                />
              ))}
              <div ref={bottomRef} />
            </div>
          </ScrollArea>
        )}
      </CardContent>
    </Card>
  );
}
