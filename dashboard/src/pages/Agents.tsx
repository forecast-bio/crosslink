import { useEffect } from "react";
import { Link } from "react-router";
import { Bot } from "lucide-react";
import { useAgentsStore } from "@/stores/agents";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { formatRelativeTime } from "@/lib/utils";
import type { AgentStatus } from "@/lib/types";

function statusVariant(status: AgentStatus) {
  switch (status) {
    case "running": return "success" as const;
    case "idle": return "warning" as const;
    case "stale": return "destructive" as const;
    case "failed": return "destructive" as const;
    default: return "secondary" as const;
  }
}

export function Agents() {
  const { agents, loading, fetch } = useAgentsStore();

  useEffect(() => { void fetch(); }, [fetch]);

  if (loading) {
    return (
      <div className="p-6">
        <h1 className="text-2xl font-bold mb-4">Agents</h1>
        <p className="text-muted-foreground">Loading…</p>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-4">
      <h1 className="text-2xl font-bold">Agents</h1>

      {agents.length === 0 ? (
        <Card>
          <CardContent className="py-12 text-center">
            <Bot className="h-10 w-10 mx-auto mb-3 text-muted-foreground/40" />
            <p className="text-muted-foreground text-sm">
              No active agents. Launch one with{" "}
              <code className="text-blue-400">crosslink kickoff run</code>
            </p>
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <Link key={agent.id} to={`/agents/${encodeURIComponent(agent.id)}`}>
              <Card className="hover:bg-accent/30 transition-colors cursor-pointer">
                <CardContent className="p-4 space-y-2">
                  <div className="flex items-center justify-between">
                    <span className="font-mono text-xs truncate text-foreground/80">
                      {agent.id}
                    </span>
                    <Badge variant={statusVariant(agent.status)}>{agent.status}</Badge>
                  </div>
                  {agent.branch && (
                    <p className="text-xs text-muted-foreground truncate">{agent.branch}</p>
                  )}
                  {agent.last_heartbeat && (
                    <p className="text-xs text-muted-foreground">
                      Last seen {formatRelativeTime(agent.last_heartbeat.timestamp)}
                    </p>
                  )}
                  {agent.active_issue_id && (
                    <p className="text-xs text-blue-400">Issue #{agent.active_issue_id}</p>
                  )}
                </CardContent>
              </Card>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
