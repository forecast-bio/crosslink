import { create } from "zustand";
import { orchestrator as orchestratorApi } from "@/api/client";
import type {
  ExecutionEvent,
  ExecutionEventKind,
  OrchestratorPlan,
  OrchestratorPhase,
  OrchestratorStage,
  StageStatus,
} from "@/lib/types";

let eventCounter = 0;

function makeEvent(
  kind: ExecutionEventKind,
  message: string,
  opts?: { phase_id?: string; stage_id?: string; agent_id?: string },
): ExecutionEvent {
  return {
    id: `evt-${++eventCounter}-${Date.now()}`,
    timestamp: new Date().toISOString(),
    kind,
    phase_id: opts?.phase_id ?? null,
    stage_id: opts?.stage_id ?? null,
    agent_id: opts?.agent_id ?? null,
    message,
  };
}

interface OrchestratorState {
  plan: OrchestratorPlan | null;
  executionStatus: string;
  progressPct: number;
  loading: boolean;
  decomposing: boolean;
  error: string | null;
  events: ExecutionEvent[];
  selectedStageId: string | null;

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
  applyProgress: (phase: string, stage: string, status: string, agentId?: string | null) => void;
  addEvent: (event: ExecutionEvent) => void;
  selectStage: (stageId: string | null) => void;
  getSelectedStage: () => OrchestratorStage | null;
  retryStage: (stageId: string) => Promise<void>;
  skipStage: (stageId: string) => Promise<void>;
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
  events: [],
  selectedStageId: null,

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
      const prev = get().executionStatus;
      const next = data.status;

      // Generate events for execution-level state transitions
      if (prev !== next) {
        const events = get().events;
        if (next === "running" && prev === "paused") {
          events.push(makeEvent("execution_resumed", "Execution resumed"));
        } else if (next === "running" && prev === "idle") {
          events.push(makeEvent("execution_started", "Execution started"));
        } else if (next === "paused") {
          events.push(makeEvent("execution_paused", "Execution paused"));
        } else if (next === "done") {
          events.push(makeEvent("execution_completed", "Execution completed successfully"));
        } else if (next === "failed") {
          events.push(makeEvent("execution_failed", "Execution failed"));
        }
        set({ events: [...events] });
      }

      set({ executionStatus: next, progressPct: data.progress_pct });
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

  applyProgress: (phase, stage, status, agentId) => {
    const plan = get().plan;
    if (!plan) return;

    // Find stage title for event messages
    let stageTitle = stage;
    for (const p of plan.phases) {
      const s = p.stages.find((s) => s.id === stage);
      if (s) {
        stageTitle = s.title;
        break;
      }
    }

    // Generate event for stage status changes
    const events = get().events;
    const agentLabel = agentId ? ` (agent: ${agentId})` : "";
    switch (status as StageStatus) {
      case "running":
        events.push(
          makeEvent("stage_started", `Stage "${stageTitle}" started${agentLabel}`, {
            phase_id: phase,
            stage_id: stage,
            agent_id: agentId ?? undefined,
          }),
        );
        break;
      case "done":
        events.push(
          makeEvent("stage_completed", `Stage "${stageTitle}" completed${agentLabel}`, {
            phase_id: phase,
            stage_id: stage,
            agent_id: agentId ?? undefined,
          }),
        );
        break;
      case "failed":
        events.push(
          makeEvent("stage_failed", `Stage "${stageTitle}" failed${agentLabel}`, {
            phase_id: phase,
            stage_id: stage,
            agent_id: agentId ?? undefined,
          }),
        );
        break;
      case "skipped":
        events.push(
          makeEvent("stage_skipped", `Stage "${stageTitle}" skipped`, {
            phase_id: phase,
            stage_id: stage,
          }),
        );
        break;
    }

    const updatedPhases = plan.phases.map((p) =>
      p.id === phase
        ? {
            ...p,
            stages: p.stages.map((s) =>
              s.id === stage
                ? { ...s, status: status as StageStatus, agent_id: agentId ?? s.agent_id }
                : s,
            ),
          }
        : p,
    );

    // Recompute progress from stage statuses
    const allStages = updatedPhases.flatMap((p) => p.stages);
    const doneCount = allStages.filter(
      (s) => s.status === "done" || s.status === "skipped",
    ).length;
    const progressPct =
      allStages.length > 0 ? Math.round((doneCount / allStages.length) * 100) : 0;

    set({
      plan: { ...plan, phases: updatedPhases },
      events: [...events],
      progressPct,
    });
  },

  addEvent: (event) => {
    set((s) => ({ events: [...s.events, event] }));
  },

  selectStage: (stageId) => set({ selectedStageId: stageId }),

  getSelectedStage: () => {
    const { plan, selectedStageId } = get();
    if (!plan || !selectedStageId) return null;
    for (const phase of plan.phases) {
      const stage = phase.stages.find((s) => s.id === selectedStageId);
      if (stage) return stage;
    }
    return null;
  },

  retryStage: async (stageId: string) => {
    const plan = get().plan;
    if (!plan) return;

    let stageTitle = stageId;
    for (const p of plan.phases) {
      const s = p.stages.find((s) => s.id === stageId);
      if (s) {
        stageTitle = s.title;
        break;
      }
    }

    await orchestratorApi.retryStage(stageId);
    const events = get().events;
    events.push(
      makeEvent("stage_retried", `Stage "${stageTitle}" retried`, { stage_id: stageId }),
    );
    set({ events: [...events] });
    void get().fetchStatus();
  },

  skipStage: async (stageId: string) => {
    const plan = get().plan;
    if (!plan) return;

    await orchestratorApi.skipStage(stageId);
    void get().fetchStatus();
  },
}));
