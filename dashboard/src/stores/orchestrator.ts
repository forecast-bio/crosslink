import { create } from "zustand";
import { orchestrator as orchestratorApi } from "@/api/client";
import type {
  OrchestratorPlan,
  OrchestratorPhase,
  OrchestratorStage,
  StageStatus,
} from "@/lib/types";

interface OrchestratorState {
  plan: OrchestratorPlan | null;
  executionStatus: string;
  progressPct: number;
  loading: boolean;
  decomposing: boolean;
  error: string | null;

  // Fetch actions
  fetchPlan: () => Promise<void>;
  fetchStatus: () => Promise<void>;

  // Decompose
  decompose: (document: string) => Promise<OrchestratorPlan | null>;

  // Plan setters
  setPlan: (plan: OrchestratorPlan) => void;
  clearPlan: () => void;

  // Plan mutation helpers for the stage editor
  updatePhase: (phaseId: string, patch: Partial<Pick<OrchestratorPhase, "title" | "description" | "gate_criteria">>) => void;
  addStage: (phaseId: string, stage: OrchestratorStage) => void;
  removeStage: (phaseId: string, stageId: string) => void;
  updateStage: (phaseId: string, stageId: string, patch: Partial<Pick<OrchestratorStage, "title" | "description" | "agent_count" | "complexity_hours">>) => void;
  addDependency: (phaseId: string, stageId: string, dependsOnStageId: string) => void;
  removeDependency: (phaseId: string, stageId: string, dependsOnStageId: string) => void;
  reorderStages: (phaseId: string, fromIndex: number, toIndex: number) => void;

  // WebSocket-driven execution progress
  applyProgress: (phase: string, stage: string, status: string) => void;
}

function mapPhases(
  plan: OrchestratorPlan,
  phaseId: string,
  fn: (phase: OrchestratorPhase) => OrchestratorPhase,
): OrchestratorPlan {
  return {
    ...plan,
    phases: plan.phases.map((p) => (p.id === phaseId ? fn(p) : p)),
  };
}

function mapStages(
  phase: OrchestratorPhase,
  stageId: string,
  fn: (stage: OrchestratorStage) => OrchestratorStage,
): OrchestratorPhase {
  return {
    ...phase,
    stages: phase.stages.map((s) => (s.id === stageId ? fn(s) : s)),
  };
}

function recomputeTotals(plan: OrchestratorPlan): OrchestratorPlan {
  let totalStages = 0;
  let totalHours = 0;
  for (const phase of plan.phases) {
    totalStages += phase.stages.length;
    for (const stage of phase.stages) {
      totalHours += stage.complexity_hours;
    }
  }
  return { ...plan, total_stages: totalStages, estimated_hours: totalHours };
}

export const useOrchestratorStore = create<OrchestratorState>((set, get) => ({
  plan: null,
  executionStatus: "idle",
  progressPct: 0,
  loading: false,
  decomposing: false,
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

  fetchStatus: async () => {
    try {
      const data = await orchestratorApi.status();
      set({ executionStatus: data.status, progressPct: data.progress_pct });
    } catch {
      // non-fatal
    }
  },

  decompose: async (document: string) => {
    set({ decomposing: true, error: null });
    try {
      const result = await orchestratorApi.decompose(document);
      set({ plan: result, decomposing: false });
      return result;
    } catch (e) {
      set({ error: String(e), decomposing: false });
      return null;
    }
  },

  setPlan: (plan) => set({ plan }),

  clearPlan: () => set({ plan: null, executionStatus: "idle", progressPct: 0 }),

  updatePhase: (phaseId, patch) => {
    const plan = get().plan;
    if (!plan) return;
    set({ plan: mapPhases(plan, phaseId, (p) => ({ ...p, ...patch })) });
  },

  addStage: (phaseId, stage) => {
    const plan = get().plan;
    if (!plan) return;
    const updated = mapPhases(plan, phaseId, (p) => ({
      ...p,
      stages: [...p.stages, stage],
    }));
    set({ plan: recomputeTotals(updated) });
  },

  removeStage: (phaseId, stageId) => {
    const plan = get().plan;
    if (!plan) return;
    const updated: OrchestratorPlan = {
      ...plan,
      phases: plan.phases.map((p) => ({
        ...p,
        stages: (p.id === phaseId
          ? p.stages.filter((s) => s.id !== stageId)
          : p.stages
        ).map((s) => ({
          ...s,
          depends_on: s.depends_on.filter((d) => d !== stageId),
        })),
      })),
    };
    set({ plan: recomputeTotals(updated) });
  },

  updateStage: (phaseId, stageId, patch) => {
    const plan = get().plan;
    if (!plan) return;
    const updated = mapPhases(plan, phaseId, (p) =>
      mapStages(p, stageId, (s) => ({ ...s, ...patch })),
    );
    set({ plan: recomputeTotals(updated) });
  },

  addDependency: (phaseId, stageId, dependsOnStageId) => {
    const plan = get().plan;
    if (!plan) return;
    set({
      plan: mapPhases(plan, phaseId, (p) =>
        mapStages(p, stageId, (s) =>
          s.depends_on.includes(dependsOnStageId)
            ? s
            : { ...s, depends_on: [...s.depends_on, dependsOnStageId] },
        ),
      ),
    });
  },

  removeDependency: (phaseId, stageId, dependsOnStageId) => {
    const plan = get().plan;
    if (!plan) return;
    set({
      plan: mapPhases(plan, phaseId, (p) =>
        mapStages(p, stageId, (s) => ({
          ...s,
          depends_on: s.depends_on.filter((d) => d !== dependsOnStageId),
        })),
      ),
    });
  },

  reorderStages: (phaseId, fromIndex, toIndex) => {
    const plan = get().plan;
    if (!plan) return;
    set({
      plan: mapPhases(plan, phaseId, (p) => {
        const stages = [...p.stages];
        const [moved] = stages.splice(fromIndex, 1);
        stages.splice(toIndex, 0, moved);
        return { ...p, stages };
      }),
    });
  },

  applyProgress: (phase, stage, status) => {
    const plan = get().plan;
    if (!plan) return;
    const updatedPhases = plan.phases.map((p) =>
      p.id === phase
        ? {
            ...p,
            stages: p.stages.map((s) =>
              s.id === stage ? { ...s, status: status as StageStatus } : s,
            ),
          }
        : p,
    );
    set({ plan: { ...plan, phases: updatedPhases } });
  },
}));
