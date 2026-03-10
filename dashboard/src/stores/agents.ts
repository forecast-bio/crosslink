import { create } from "zustand";
import { agents as agentsApi } from "@/api/client";
import type { Agent, AgentStatus } from "@/lib/types";

interface AgentsState {
  agents: Agent[];
  loading: boolean;
  error: string | null;
  fetch: () => Promise<void>;
  applyHeartbeat: (agentId: string, timestamp: string, issueId?: number) => void;
  applyStatus: (agentId: string, status: AgentStatus) => void;
}

export const useAgentsStore = create<AgentsState>((set, get) => ({
  agents: [],
  loading: false,
  error: null,

  fetch: async () => {
    set({ loading: true, error: null });
    try {
      const data = await agentsApi.list();
      set({ agents: data, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  applyHeartbeat: (agentId, timestamp, issueId) => {
    const existing = get().agents.find((a) => a.agent_id === agentId);
    if (existing) {
      set((s) => ({
        agents: s.agents.map((a) =>
          a.agent_id === agentId
            ? {
                ...a,
                status: "active" as AgentStatus,
                active_issue_id: issueId ?? a.active_issue_id,
                last_heartbeat: timestamp,
              }
            : a,
        ),
      }));
    } else {
      // Unknown agent — refetch to get full details
      void get().fetch();
    }
  },

  applyStatus: (agentId, status) => {
    set((s) => ({
      agents: s.agents.map((a) =>
        a.agent_id === agentId ? { ...a, status } : a,
      ),
    }));
  },
}));
