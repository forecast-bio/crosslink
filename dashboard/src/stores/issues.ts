import { create } from "zustand";
import { issues as issuesApi, type IssueListParams } from "@/api/client";
import type { Issue, IssueDetail, IssuePriority } from "@/lib/types";

interface IssuesState {
  issues: Issue[];
  detail: Record<number, IssueDetail>;
  loading: boolean;
  error: string | null;
  fetch: (params?: IssueListParams) => Promise<void>;
  fetchDetail: (id: number) => Promise<void>;
  create: (data: { title: string; description?: string; priority?: IssuePriority }) => Promise<Issue>;
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

  create: async (data) => {
    const issue = await issuesApi.create(data);
    await get().fetch();
    return issue;
  },

  invalidate: (id) => {
    set((s) => {
      const next = { ...s.detail };
      delete next[id];
      return { detail: next };
    });
    void get().fetch();
  },
}));
