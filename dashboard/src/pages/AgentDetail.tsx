import { useEffect, useState } from "react";
import { useParams, Link } from "react-router";
import { ArrowLeft } from "lucide-react";
import { agents as agentsApi } from "@/api/client";
import { useAgentsStore } from "@/stores/agents";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatRelativeTime, formatDateTime } from "@/lib/utils";
import type { Agent } from "@/lib/types";

export function AgentDetail() {
  const { id } = useParams<{ id: string }>();
  const agentsFromStore = useAgentsStore((s) => s.agents);
  const [agent, setAgent] = useState<Agent | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!id) return;
    const fromStore = agentsFromStore.find((a) => a.id === id);
    if (fromStore) setAgent(fromStore);
    agentsApi
      .get(id)
      .then(setAgent)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, [id, agentsFromStore]);

  if (loading && !agent) {
    return <div className="p-6 text-muted-foreground">Loading…</div>;
  }

  if (!agent) {
    return (
      <div className="p-6">
        <p className="text-muted-foreground">Agent not found.</p>
        <Link to="/agents">
          <Button variant="ghost" size="sm" className="mt-2">
            <ArrowLeft className="h-4 w-4 mr-1" /> Back
          </Button>
        </Link>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center gap-3">
        <Link to="/agents">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-4 w-4" />
          </Button>
        </Link>
        <h1 className="text-xl font-bold font-mono truncate">{agent.id}</h1>
        <Badge
          variant={
            agent.status === "running"
              ? "success"
              : agent.status === "idle"
                ? "warning"
                : "secondary"
          }
        >
          {agent.status}
        </Badge>
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Details</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 text-sm">
            {agent.branch && (
              <div className="flex justify-between">
                <span className="text-muted-foreground">Branch</span>
                <span className="font-mono text-xs">{agent.branch}</span>
              </div>
            )}
            {agent.worktree_path && (
              <div className="flex justify-between">
                <span className="text-muted-foreground">Worktree</span>
                <span className="font-mono text-xs truncate max-w-40">{agent.worktree_path}</span>
              </div>
            )}
            {agent.tmux_session && (
              <div className="flex justify-between">
                <span className="text-muted-foreground">Tmux</span>
                <span className="font-mono text-xs">{agent.tmux_session}</span>
              </div>
            )}
            {agent.active_issue_id && (
              <div className="flex justify-between">
                <span className="text-muted-foreground">Active Issue</span>
                <Link
                  to={`/issues/${agent.active_issue_id}`}
                  className="text-blue-400 hover:underline text-xs"
                >
                  #{agent.active_issue_id}
                </Link>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Last Heartbeat</CardTitle>
          </CardHeader>
          <CardContent className="text-sm">
            {agent.last_heartbeat ? (
              <div className="space-y-2">
                <div className="flex justify-between">
                  <span className="text-muted-foreground">Time</span>
                  <span>{formatRelativeTime(agent.last_heartbeat.timestamp)}</span>
                </div>
                <div className="flex justify-between">
                  <span className="text-muted-foreground">Exact</span>
                  <span className="text-xs text-muted-foreground">
                    {formatDateTime(agent.last_heartbeat.timestamp)}
                  </span>
                </div>
                {agent.last_heartbeat.message && (
                  <p className="text-xs text-muted-foreground border-t border-border pt-2 mt-2">
                    {agent.last_heartbeat.message}
                  </p>
                )}
              </div>
            ) : (
              <p className="text-muted-foreground">No heartbeat recorded</p>
            )}
          </CardContent>
        </Card>
      </div>

      {agent.locks.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Held Locks</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-2">
              {agent.locks.map((lock) => (
                <div key={lock.issue_id} className="flex items-center justify-between text-sm">
                  <Link to={`/issues/${lock.issue_id}`} className="text-blue-400 hover:underline">
                    Issue #{lock.issue_id}
                  </Link>
                  <div className="flex items-center gap-2">
                    {lock.stale && <Badge variant="destructive">stale</Badge>}
                    <span className="text-xs text-muted-foreground">
                      {Math.round(lock.age_seconds / 60)}m ago
                    </span>
                  </div>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
