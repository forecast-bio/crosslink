import { create } from "zustand";
import { usage as usageApi } from "@/api/client";
import type { BudgetConfig, UsageSummary } from "@/lib/types";

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
      const data = await usageApi.summary(params);
      set({ summary: data, loading: false });
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
