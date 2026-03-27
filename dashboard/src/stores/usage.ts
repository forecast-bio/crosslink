import { create } from "zustand";
import { usage as usageApi } from "@/api/client";
import type {
  AgentUsageSummary,
  BudgetConfig,
  ModelUsageSummary,
  RawUsageSummary,
  UsageSummary,
} from "@/lib/types";

/** Transform raw API response into aggregated UsageSummary. */
function aggregateUsageSummary(raw: RawUsageSummary): UsageSummary {
  // Aggregate items by agent
  const agentMap = new Map<string, AgentUsageSummary>();
  for (const r of raw.items) {
    const existing = agentMap.get(r.agent_id);
    if (existing) {
      existing.input_tokens += r.total_input_tokens;
      existing.output_tokens += r.total_output_tokens;
      existing.cost_estimate += r.total_cost;
      existing.interaction_count += r.request_count;
    } else {
      agentMap.set(r.agent_id, {
        agent_id: r.agent_id,
        input_tokens: r.total_input_tokens,
        output_tokens: r.total_output_tokens,
        cost_estimate: r.total_cost,
        interaction_count: r.request_count,
      });
    }
  }

  // Aggregate items by model
  const modelMap = new Map<string, ModelUsageSummary>();
  for (const r of raw.items) {
    const existing = modelMap.get(r.model);
    if (existing) {
      existing.input_tokens += r.total_input_tokens;
      existing.output_tokens += r.total_output_tokens;
      existing.cost_estimate += r.total_cost;
    } else {
      modelMap.set(r.model, {
        model: r.model,
        input_tokens: r.total_input_tokens,
        output_tokens: r.total_output_tokens,
        cost_estimate: r.total_cost,
      });
    }
  }

  return {
    total_input_tokens: raw.total_input_tokens,
    total_output_tokens: raw.total_output_tokens,
    total_cost: raw.total_cost,
    by_agent: [...agentMap.values()],
    by_model: [...modelMap.values()],
    daily: [],
  };
}

interface UsageState {
  summary: UsageSummary | null;
  budget: BudgetConfig | null;
  loading: boolean;
  error: string | null;
  fetchSummary: (params?: { agent_id?: string; from?: string; to?: string }) => Promise<void>;
  fetchBudget: () => Promise<void>;
  updateBudget: (data: Partial<BudgetConfig>) => Promise<void>;
}

export const useUsageStore = create<UsageState>((set, get) => ({
  summary: null,
  budget: null,
  loading: false,
  error: null,

  fetchSummary: async (params) => {
    set({ loading: true, error: null });
    try {
      const raw = await usageApi.summary(params);
      set({ summary: aggregateUsageSummary(raw), loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  fetchBudget: async () => {
    try {
      const data = await usageApi.budget();
      set({ budget: data });
    } catch {
      // Budget endpoint may not exist yet — default values
      if (!get().budget) {
        set({
          budget: {
            daily_limit: null,
            monthly_limit: null,
            alert_threshold_percent: 80,
          },
        });
      }
    }
  },

  updateBudget: async (data) => {
    try {
      const updated = await usageApi.updateBudget(data);
      set({ budget: updated });
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));
