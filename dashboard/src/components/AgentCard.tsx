import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { formatRelativeTime } from "@/lib/utils";
import type { Agent, AgentStatus } from "@/lib/types";

function statusBadgeVariant(status: AgentStatus) {
  switch (status) {
    case "active":  return "success" as const;
    case "idle":    return "warning" as const;
    case "stale":   return "destructive" as const;
    case "failed":  return "destructive" as const;
    case "done":    return "secondary" as const;
    default:        return "secondary" as const;
  }
}

function statusDotClass(status: AgentStatus): string {
  switch (status) {
    case "active":  return "bg-green-500";
    case "idle":    return "bg-yellow-400";
    case "stale":   return "bg-red-500";
    case "failed":  return "bg-red-500";
    case "done":    return "bg-zinc-500";
    default:        return "bg-zinc-500";
  }
}

function statusLabel(status: AgentStatus): string {
  switch (status) {
    case "active":  return "running";
    default:        return status;
  }
}

interface AgentCardProps {
  agent: Agent;
}

export function AgentCard({ agent }: AgentCardProps) {
  const navigate = useNavigate();
  const [pulsing, setPulsing] = useState(false);
  const prevHeartbeat = useRef(agent.last_heartbeat?.timestamp);

  // Flash a brief pulse animation whenever the heartbeat timestamp changes
  useEffect(() => {
    const ts = agent.last_heartbeat?.timestamp;
    if (ts && ts !== prevHeartbeat.current) {
      prevHeartbeat.current = ts;
      setPulsing(true);
      const t = setTimeout(() => setPulsing(false), 800);
      return () => clearTimeout(t);
    }
  }, [agent.last_heartbeat?.timestamp]);

  const displayId = agent.description ?? agent.id;

  return (
    <Card
      className={[
        "cursor-pointer transition-all duration-200",
        "hover:bg-accent/30 hover:shadow-md",
        pulsing ? "ring-1 ring-green-500/60" : "",
      ].join(" ")}
      onClick={() => void navigate(`/agents/${encodeURIComponent(agent.id)}`)}
    >
      <CardContent className="p-4 space-y-2">
        {/* Header row: status dot + id + badge */}
        <div className="flex items-center gap-2">
          <span
            className={[
              "h-2 w-2 rounded-full shrink-0",
              statusDotClass(agent.status),
              agent.status === "active" ? "animate-pulse" : "",
            ].join(" ")}
          />
          <span className="font-mono text-xs truncate flex-1 text-foreground/80" title={agent.id}>
            {displayId}
          </span>
          <Badge variant={statusBadgeVariant(agent.status)} className="shrink-0 text-xs">
            {statusLabel(agent.status)}
          </Badge>
        </div>

        {/* Branch */}
        {agent.branch && (
          <p className="text-xs text-muted-foreground truncate pl-4" title={agent.branch}>
            {agent.branch}
          </p>
        )}

        {/* Last heartbeat */}
        {agent.last_heartbeat && (
          <p className="text-xs text-muted-foreground pl-4">
            Last seen {formatRelativeTime(agent.last_heartbeat.timestamp)}
          </p>
        )}

        {/* Active issue */}
        {agent.active_issue_id != null && (
          <p className="text-xs text-blue-400 pl-4">Issue #{agent.active_issue_id}</p>
        )}
      </CardContent>
    </Card>
  );
}
