import { useEffect, useState } from "react";
import { useParams, Link } from "react-router";
import {
  ArrowLeft,
  Clock,
  GitBranch,
  FolderOpen,
  Terminal,
  AlertCircle,
  FileText,
} from "lucide-react";
import { agents as agentsApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { HeartbeatTimeline } from "@/components/HeartbeatTimeline";
import { LockList } from "@/components/LockList";
import { formatRelativeTime, formatDateTime } from "@/lib/utils";
import type { AgentDetailResponse } from "@/lib/types";

function statusVariant(
  status: AgentDetailResponse["status"],
): "success" | "warning" | "destructive" | "secondary" {
  switch (status) {
    case "running":
    case "active":
      return "success";
    case "idle":
      return "warning";
    case "stale":
    case "failed":
      return "destructive";
    default:
      return "secondary";
  }
}

export function AgentDetail() {
  const { id } = useParams<{ id: string }>();
  const [agent, setAgent] = useState<AgentDetailResponse | null>(null);
  const [kickoffReport, setKickoffReport] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!id) return;

    setError(null);
    agentsApi
      .get(id)
      .then((data) => {
        setAgent(data);
        // If the detail response already includes the report, use it
        if (data.kickoff_report) {
          setKickoffReport(data.kickoff_report);
        }
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));

    // Fetch kickoff status separately (may have a fuller report)
    agentsApi
      .getStatus(id)
      .then((data) => {
        if (data.report) setKickoffReport(data.report);
      })
      .catch(() => {
        // Non-critical: kickoff status may not exist for all agents
      });
  }, [id]);

  if (loading && !agent) {
    return <div className="p-6 text-muted-foreground">Loading…</div>;
  }

  if (error && !agent) {
    return (
      <div className="p-6">
        <p className="text-destructive text-sm">{error}</p>
        <Link to="/agents">
          <Button variant="ghost" size="sm" className="mt-2">
            <ArrowLeft className="h-4 w-4 mr-1" /> Back
          </Button>
        </Link>
      </div>
    );
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

  const kickoffStatus = agent.kickoff_status;

  return (
    <div className="p-6 space-y-4 max-w-4xl">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Link to="/agents">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-4 w-4" />
          </Button>
        </Link>
        <h1 className="text-xl font-bold font-mono truncate flex-1">{agent.agent_id}</h1>
        <Badge variant={statusVariant(agent.status)}>{agent.status}</Badge>
      </div>

      {/* Metadata */}
      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Agent Details</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3 text-sm">
            {agent.branch && (
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground flex items-center gap-1.5">
                  <GitBranch className="h-3 w-3" />
                  Branch
                </span>
                <span className="font-mono text-xs truncate max-w-52" title={agent.branch}>
                  {agent.branch}
                </span>
              </div>
            )}
            {agent.worktree_path && (
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground flex items-center gap-1.5">
                  <FolderOpen className="h-3 w-3" />
                  Worktree
                </span>
                <span
                  className="font-mono text-xs truncate max-w-52"
                  title={agent.worktree_path}
                >
                  {agent.worktree_path.split("/").slice(-2).join("/")}
                </span>
              </div>
            )}
            {agent.tmux_session && (
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground flex items-center gap-1.5">
                  <Terminal className="h-3 w-3" />
                  Session
                </span>
                <span className="font-mono text-xs">{agent.tmux_session}</span>
              </div>
            )}
            {agent.active_issue_id != null && (
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Active Issue</span>
                <Link
                  to={`/issues/${agent.active_issue_id}`}
                  className="text-blue-400 hover:underline font-mono text-xs"
                >
                  #{agent.active_issue_id}
                </Link>
              </div>
            )}
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">Machine</span>
              <span className="font-mono text-xs text-muted-foreground">{agent.machine_id}</span>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm flex items-center gap-1.5">
              <Clock className="h-3.5 w-3.5" />
              Last Heartbeat
            </CardTitle>
          </CardHeader>
          <CardContent className="text-sm">
            {agent.last_heartbeat ? (
              <div className="space-y-2">
                <div className="flex justify-between">
                  <span className="text-muted-foreground">Time</span>
                  <span className="font-medium">
                    {formatRelativeTime(agent.last_heartbeat.timestamp)}
                  </span>
                </div>
                <div className="flex justify-between">
                  <span className="text-muted-foreground">Exact</span>
                  <span className="text-xs text-muted-foreground">
                    {formatDateTime(agent.last_heartbeat.timestamp)}
                  </span>
                </div>
                {agent.last_heartbeat.issue_id != null && (
                  <div className="flex justify-between">
                    <span className="text-muted-foreground">Issue</span>
                    <Link
                      to={`/issues/${agent.last_heartbeat.issue_id}`}
                      className="text-blue-400 hover:underline font-mono text-xs"
                    >
                      #{agent.last_heartbeat.issue_id}
                    </Link>
                  </div>
                )}
                {agent.last_heartbeat.message && (
                  <p className="text-xs text-muted-foreground border-t border-border pt-2 mt-1 break-words">
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

      {/* Heartbeat Timeline */}
      <Card>
        <CardHeader>
          <CardTitle className="text-sm flex items-center gap-1.5">
            <Clock className="h-3.5 w-3.5" />
            Heartbeat Timeline (last 24h)
          </CardTitle>
        </CardHeader>
        <CardContent>
          <HeartbeatTimeline timestamps={agent.heartbeat_history} />
        </CardContent>
      </Card>

      {/* Held Locks */}
      <Card>
        <CardHeader>
          <CardTitle className="text-sm">Held Locks</CardTitle>
        </CardHeader>
        <CardContent>
          <LockList locks={agent.locks} />
        </CardContent>
      </Card>

      {/* Kickoff Status */}
      {kickoffStatus && (
        <Card>
          <CardHeader>
            <CardTitle className="text-sm flex items-center gap-1.5">
              <AlertCircle className="h-3.5 w-3.5" />
              Kickoff Status
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-sm font-mono whitespace-pre-wrap">{kickoffStatus}</p>
          </CardContent>
        </Card>
      )}

      {/* Kickoff Report */}
      {kickoffReport && (
        <Card>
          <CardHeader>
            <CardTitle className="text-sm flex items-center gap-1.5">
              <FileText className="h-3.5 w-3.5" />
              Kickoff Report
            </CardTitle>
          </CardHeader>
          <CardContent>
            <ScrollArea className="h-64 w-full rounded border border-border">
              <pre className="p-3 text-xs font-mono whitespace-pre-wrap text-muted-foreground leading-relaxed">
                {kickoffReport}
              </pre>
            </ScrollArea>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
