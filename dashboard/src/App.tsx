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
import { CommandPalette } from "@/components/CommandPalette";
import { Usage } from "@/pages/Usage";
import { wsClient } from "@/api/ws";
import { useAgentsStore } from "@/stores/agents";
import { useIssuesStore } from "@/stores/issues";
import { useOrchestratorStore } from "@/stores/orchestrator";

function WsListener() {
  const { applyHeartbeat, applyStatus } = useAgentsStore();
  const { invalidate } = useIssuesStore();
  const { applyProgress } = useOrchestratorStore();

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
        case "execution_progress":
          applyProgress(msg.phase_id, msg.stage_id, msg.status, msg.agent_id);
          break;
        default:
          break;
      }
    });
    return () => {
      off();
      wsClient.disconnect();
    };
  }, [applyHeartbeat, applyStatus, invalidate, applyProgress]);

  return null;
}

export function App() {
  return (
    <BrowserRouter>
      <WsListener />
      <CommandPalette />
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
            <Route path="/usage" element={<Usage />} />
          </Routes>
        </main>
      </div>
    </BrowserRouter>
  );
}
