import { create } from "zustand";
import { orchestrator as orchestratorApi } from "@/api/client";
import type { OrchestratorPlan, StageStatus } from "@/lib/types";

interface OrchestratorState {
  plan: OrchestratorPlan | null;
  executionStatus: string;
  progressPct: number;
  loading: boolean;
  error: string | null;
  fetchPlan: () => Promise<void>;
  setPlan: (plan: OrchestratorPlan) => void;
  fetchStatus: () => Promise<void>;
  applyProgress: (phase: string, stage: string, status: string) => void;
}

export const useOrchestratorStore = create<OrchestratorState>((set, get) => ({
  plan: null,
  executionStatus: "idle",
  progressPct: 0,
  loading: false,
  error: null,

  fetchPlan: async () => {
    set({ loading: true, error: null });
    try {
      const data = await orchestratorApi.getPlan();
      set({ plan: data, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  setPlan: (plan) => set({ plan }),

  fetchStatus: async () => {
    try {
      const data = await orchestratorApi.status();
      set({ executionStatus: data.status, progressPct: data.progress_pct });
    } catch {
      // non-fatal
    }
  },

  applyProgress: (phase, stage, status) => {
    const plan = get().plan;
    if (!plan) return;
    const updatedPhases = plan.phases.map((p) =>
      p.id === phase
        ? {
            ...p,
            stages: p.stages.map((s) =>
              s.id === stage
                ? { ...s, status: status as StageStatus }
                : s,
            ),
          }
        : p,
    );
    set({ plan: { ...plan, phases: updatedPhases } });
  },
}));
