import { create } from "zustand";
import { issues as issuesApi } from "@/api/client";
import type { Issue, IssueDetail } from "@/lib/types";

interface IssuesState {
  issues: Issue[];
  detail: Record<number, IssueDetail>;
  loading: boolean;
  error: string | null;
  fetch: (params?: Parameters<typeof issuesApi.list>[0]) => Promise<void>;
  fetchDetail: (id: number) => Promise<void>;
  invalidate: (id: number) => void;
}

export const useIssuesStore = create<IssuesState>((set, get) => ({
  issues: [],
  detail: {},
  loading: false,
  error: null,

  fetch: async (params) => {
    set({ loading: true, error: null });
    try {
      const data = await issuesApi.list(params);
      set({ issues: data, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  fetchDetail: async (id) => {
    try {
      const data = await issuesApi.get(id);
      set((s) => ({ detail: { ...s.detail, [id]: data } }));
    } catch (e) {
      set({ error: String(e) });
    }
  },

  invalidate: (id) => {
    // Remove cached detail so next access re-fetches
    set((s) => {
      const next = { ...s.detail };
      delete next[id];
      return { detail: next };
    });
    // Also update the list entry if present
    void get().fetch();
  },
}));
