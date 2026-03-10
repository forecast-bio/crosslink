import { useEffect } from "react";
import { BrowserRouter, Routes, Route } from "react-router";
import { Sidebar } from "@/components/Sidebar";
import { Dashboard } from "@/pages/Dashboard";
import { Agents } from "@/pages/Agents";
import { AgentDetail } from "@/pages/AgentDetail";
import { Issues } from "@/pages/Issues";
import { IssueDetail } from "@/pages/IssueDetail";
import { Sessions } from "@/pages/Sessions";
import { Milestones } from "@/pages/Milestones";
import { Knowledge } from "@/pages/Knowledge";
import { KnowledgeDetail } from "@/pages/KnowledgeDetail";
import { Sync } from "@/pages/Sync";
import { Config } from "@/pages/Config";
import { Orchestrator } from "@/pages/Orchestrator";
import { Execution } from "@/pages/Execution";
import { wsClient } from "@/api/ws";
import { useAgentsStore } from "@/stores/agents";
import { useIssuesStore } from "@/stores/issues";

function WsListener() {
  const { applyHeartbeat, applyStatus } = useAgentsStore();
  const { invalidate } = useIssuesStore();

  useEffect(() => {
    wsClient.connect(["agents", "issues", "execution"]);
    const off = wsClient.on((msg) => {
      switch (msg.type) {
        case "heartbeat":
          applyHeartbeat(msg.agent_id, msg.timestamp, msg.active_issue_id ?? undefined);
          break;
        case "agent_status":
          applyStatus(msg.agent_id, msg.status);
          break;
        case "issue_updated":
          invalidate(msg.issue_id);
          break;
        default:
          break;
      }
    });
    return () => {
      off();
      wsClient.disconnect();
    };
  }, [applyHeartbeat, applyStatus, invalidate]);

  return null;
}

export function App() {
  return (
    <BrowserRouter>
      <WsListener />
      <div className="flex h-screen overflow-hidden bg-background text-foreground">
        <Sidebar />
        <main className="flex-1 overflow-y-auto">
          <Routes>
            <Route path="/" element={<Dashboard />} />
            <Route path="/agents" element={<Agents />} />
            <Route path="/agents/:id" element={<AgentDetail />} />
            <Route path="/issues" element={<Issues />} />
            <Route path="/issues/:id" element={<IssueDetail />} />
            <Route path="/sessions" element={<Sessions />} />
            <Route path="/milestones" element={<Milestones />} />
            <Route path="/knowledge" element={<Knowledge />} />
            <Route path="/knowledge/:slug" element={<KnowledgeDetail />} />
            <Route path="/sync" element={<Sync />} />
            <Route path="/config" element={<Config />} />
            <Route path="/orchestrator" element={<Orchestrator />} />
            <Route path="/execution" element={<Execution />} />
          </Routes>
        </main>
      </div>
    </BrowserRouter>
  );
}
