import { useEffect, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { health, agents as agentsApi, issues as issuesApi, knowledge as knowledgeApi, sessions as sessionsApi } from "@/api/client";
import { Bot, CircleDot, BookOpen, Activity } from "lucide-react";

interface StatCardData {
  label: string;
  value: string | number;
  icon: React.ComponentType<{ className?: string }>;
  description: string;
}

export function Dashboard() {
  const [serverVersion, setServerVersion] = useState<string | null>(null);
  const [agentCount, setAgentCount] = useState<number | null>(null);
  const [issueCount, setIssueCount] = useState<number | null>(null);
  const [knowledgeCount, setKnowledgeCount] = useState<number | null>(null);
  const [sessionActive, setSessionActive] = useState<boolean | null>(null);

  useEffect(() => {
    health
      .get()
      .then((r) => setServerVersion(r.version))
      .catch(() => setServerVersion(null));

    agentsApi.list()
      .then((agents) => setAgentCount(agents.filter((a) => a.status === "running" || a.status === "active").length))
      .catch(() => setAgentCount(null));

    issuesApi.list({ status: "open" })
      .then((issues) => setIssueCount(issues.length))
      .catch(() => setIssueCount(null));

    knowledgeApi.list()
      .then((pages) => setKnowledgeCount(pages.length))
      .catch(() => setKnowledgeCount(null));

    sessionsApi.current()
      .then((session) => setSessionActive(session !== null))
      .catch(() => setSessionActive(null));
  }, []);

  const stats: StatCardData[] = [
    { label: "Active Agents", value: agentCount ?? "...", icon: Bot, description: "agents with recent heartbeats" },
    { label: "Open Issues", value: issueCount ?? "...", icon: CircleDot, description: "unresolved issues" },
    { label: "Knowledge Pages", value: knowledgeCount ?? "...", icon: BookOpen, description: "in knowledge repo" },
    { label: "Active Sessions", value: sessionActive === null ? "..." : sessionActive ? "1" : "0", icon: Activity, description: "running sessions" },
  ];

  return (
    <div className="p-6 space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">Dashboard</h1>
          <p className="text-muted-foreground text-sm mt-1">
            Crosslink agent monitoring and control
          </p>
        </div>
        {serverVersion !== null ? (
          <Badge variant="success">Server v{serverVersion}</Badge>
        ) : (
          <Badge variant="destructive">Server offline</Badge>
        )}
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        {stats.map((s) => (
          <Card key={s.label}>
            <CardHeader className="flex flex-row items-center justify-between pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                {s.label}
              </CardTitle>
              <s.icon className="h-4 w-4 text-muted-foreground" />
            </CardHeader>
            <CardContent>
              <p className="text-2xl font-bold">{s.value}</p>
              <p className="text-xs text-muted-foreground mt-1">{s.description}</p>
            </CardContent>
          </Card>
        ))}
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Getting Started</CardTitle>
        </CardHeader>
        <CardContent className="space-y-2 text-sm text-muted-foreground">
          <p>
            Start <code className="text-blue-400">crosslink serve --port 3100</code> to connect the
            backend, then use the sidebar to navigate.
          </p>
          <p>
            Launch agents with{" "}
            <code className="text-blue-400">crosslink kickoff run &lt;issue-id&gt;</code> and watch
            them appear in the Agents tab.
          </p>
        </CardContent>
      </Card>
    </div>
  );
}
