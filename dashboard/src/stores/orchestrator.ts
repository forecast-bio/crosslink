import { create } from "zustand";
import { orchestrator as orchestratorApi } from "@/api/client";
import type {
  ExecutionEvent,
  ExecutionEventKind,
  OrchestratorPlan,
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
  error: string | null;
  events: ExecutionEvent[];
  selectedStageId: string | null;

  fetchPlan: () => Promise<void>;
  setPlan: (plan: OrchestratorPlan) => void;
  fetchStatus: () => Promise<void>;
  applyProgress: (phase: string, stage: string, status: string, agentId?: string | null) => void;
  addEvent: (event: ExecutionEvent) => void;
  selectStage: (stageId: string | null) => void;
  getSelectedStage: () => OrchestratorStage | null;
  retryStage: (stageId: string) => Promise<void>;
  skipStage: (stageId: string) => Promise<void>;
}

export const useOrchestratorStore = create<OrchestratorState>((set, get) => ({
  plan: null,
  executionStatus: "idle",
  progressPct: 0,
  loading: false,
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

  setPlan: (plan) => set({ plan }),

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
