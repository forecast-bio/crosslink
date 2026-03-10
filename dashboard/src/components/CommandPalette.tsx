import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import {
  CircleDot,
  BookOpen,
  Bot,
  Target,
  Search,
  LayoutDashboard,
  Activity,
  RefreshCw,
  Settings,
  Layers,
  GitFork,
  Plus,
} from "lucide-react";
import {
  Dialog,
  DialogContent,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { issues as issuesApi, knowledge as knowledgeApi, agents as agentsApi } from "@/api/client";
import type { Issue, KnowledgePage, Agent } from "@/lib/types";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface PaletteItem {
  id: string;
  label: string;
  sublabel?: string;
  icon: React.ComponentType<{ className?: string }>;
  path: string;
  category: "navigate" | "issue" | "knowledge" | "agent" | "action";
}

// ---------------------------------------------------------------------------
// Static navigation items
// ---------------------------------------------------------------------------

const NAV_ITEMS: PaletteItem[] = [
  { id: "nav-dashboard", label: "Dashboard", icon: LayoutDashboard, path: "/", category: "navigate" },
  { id: "nav-agents", label: "Agents", icon: Bot, path: "/agents", category: "navigate" },
  { id: "nav-issues", label: "Issues", icon: CircleDot, path: "/issues", category: "navigate" },
  { id: "nav-sessions", label: "Sessions", icon: Activity, path: "/sessions", category: "navigate" },
  { id: "nav-milestones", label: "Milestones", icon: Target, path: "/milestones", category: "navigate" },
  { id: "nav-knowledge", label: "Knowledge", icon: BookOpen, path: "/knowledge", category: "navigate" },
  { id: "nav-sync", label: "Sync", icon: RefreshCw, path: "/sync", category: "navigate" },
  { id: "nav-orchestrator", label: "Orchestrator", icon: Layers, path: "/orchestrator", category: "navigate" },
  { id: "nav-execution", label: "Execution", icon: GitFork, path: "/execution", category: "navigate" },
  { id: "nav-config", label: "Config", icon: Settings, path: "/config", category: "navigate" },
];

const ACTION_ITEMS: PaletteItem[] = [
  { id: "action-new-issue", label: "Create new issue", icon: Plus, path: "/issues?new=1", category: "action" },
  { id: "action-new-knowledge", label: "Create knowledge page", icon: Plus, path: "/knowledge?new=1", category: "action" },
  { id: "action-new-milestone", label: "Create milestone", icon: Plus, path: "/milestones?new=1", category: "action" },
];

// ---------------------------------------------------------------------------
// Fuzzy match
// ---------------------------------------------------------------------------

function fuzzyMatch(query: string, text: string): number {
  const q = query.toLowerCase();
  const t = text.toLowerCase();

  // Exact substring match gets highest score
  if (t.includes(q)) return 100 + (q.length / t.length) * 50;

  // Fuzzy: each query char must appear in order
  let qi = 0;
  let score = 0;
  let lastIdx = -1;
  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] === q[qi]) {
      score += 10;
      // Consecutive matches score higher
      if (ti === lastIdx + 1) score += 5;
      // Word boundary bonus
      if (ti === 0 || t[ti - 1] === " " || t[ti - 1] === "-" || t[ti - 1] === "/") score += 3;
      lastIdx = ti;
      qi++;
    }
  }

  // All query chars must match
  if (qi < q.length) return 0;
  return score;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function CommandPalette() {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [selectedIdx, setSelectedIdx] = useState(0);
  const [dynamicItems, setDynamicItems] = useState<PaletteItem[]>([]);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const navigate = useNavigate();

  // Global keyboard shortcut: Cmd+K / Ctrl+K
  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        setOpen((prev) => !prev);
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  // Focus input when dialog opens
  useEffect(() => {
    if (open) {
      setQuery("");
      setSelectedIdx(0);
      setDynamicItems([]);
      // Small delay to let dialog render
      setTimeout(() => inputRef.current?.focus(), 50);
    }
  }, [open]);

  // Fetch dynamic items when query changes (debounced)
  useEffect(() => {
    if (!open || query.length < 2) {
      setDynamicItems([]);
      return;
    }

    const timer = setTimeout(() => {
      void fetchDynamicItems(query);
    }, 200);
    return () => clearTimeout(timer);
  }, [query, open]);

  async function fetchDynamicItems(q: string) {
    const results: PaletteItem[] = [];

    // Fetch in parallel
    const [issuesResult, knowledgeResult, agentsResult] = await Promise.allSettled([
      issuesApi.list({ search: q }),
      knowledgeApi.list(),
      agentsApi.list(),
    ]);

    if (issuesResult.status === "fulfilled") {
      const matched = (issuesResult.value as Issue[]).slice(0, 5);
      for (const issue of matched) {
        results.push({
          id: `issue-${issue.id}`,
          label: `#${issue.id} ${issue.title}`,
          sublabel: `${issue.status} · ${issue.priority}`,
          icon: CircleDot,
          path: `/issues/${issue.id}`,
          category: "issue",
        });
      }
    }

    if (knowledgeResult.status === "fulfilled") {
      const pages = knowledgeResult.value as KnowledgePage[];
      const matched = pages
        .map((p) => ({ page: p, score: fuzzyMatch(q, `${p.title} ${p.slug} ${p.tags.join(" ")}`) }))
        .filter((r) => r.score > 0)
        .sort((a, b) => b.score - a.score)
        .slice(0, 5);
      for (const { page } of matched) {
        results.push({
          id: `knowledge-${page.slug}`,
          label: page.title,
          sublabel: page.tags.join(", "),
          icon: BookOpen,
          path: `/knowledge/${encodeURIComponent(page.slug)}`,
          category: "knowledge",
        });
      }
    }

    if (agentsResult.status === "fulfilled") {
      const agents = agentsResult.value as Agent[];
      const matched = agents
        .map((a) => ({ agent: a, score: fuzzyMatch(q, `${a.agent_id} ${a.description ?? ""} ${a.branch ?? ""}`) }))
        .filter((r) => r.score > 0)
        .sort((a, b) => b.score - a.score)
        .slice(0, 3);
      for (const { agent } of matched) {
        results.push({
          id: `agent-${agent.agent_id}`,
          label: agent.agent_id,
          sublabel: agent.description ?? agent.branch ?? undefined,
          icon: Bot,
          path: `/agents/${encodeURIComponent(agent.agent_id)}`,
          category: "agent",
        });
      }
    }

    setDynamicItems(results);
  }

  // Build filtered list
  const allStatic = [...NAV_ITEMS, ...ACTION_ITEMS];
  const filteredStatic = query
    ? allStatic
        .map((item) => ({ item, score: fuzzyMatch(query, `${item.label} ${item.sublabel ?? ""}`) }))
        .filter((r) => r.score > 0)
        .sort((a, b) => b.score - a.score)
        .map((r) => r.item)
    : allStatic;

  const results = [...filteredStatic, ...dynamicItems];

  // Clamp selection
  const clampedIdx = Math.min(selectedIdx, Math.max(results.length - 1, 0));

  const selectItem = useCallback(
    (item: PaletteItem) => {
      setOpen(false);
      navigate(item.path);
    },
    [navigate],
  );

  // Keyboard navigation
  function handleKeyDown(e: React.KeyboardEvent) {
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        setSelectedIdx((prev) => Math.min(prev + 1, results.length - 1));
        break;
      case "ArrowUp":
        e.preventDefault();
        setSelectedIdx((prev) => Math.max(prev - 1, 0));
        break;
      case "Enter":
        e.preventDefault();
        if (results[clampedIdx]) {
          selectItem(results[clampedIdx]);
        }
        break;
      case "Escape":
        e.preventDefault();
        setOpen(false);
        break;
    }
  }

  // Scroll selected item into view
  useEffect(() => {
    if (!listRef.current) return;
    const selected = listRef.current.querySelector(`[data-idx="${clampedIdx}"]`);
    selected?.scrollIntoView({ block: "nearest" });
  }, [clampedIdx]);

  // Reset selection when results change
  useEffect(() => {
    setSelectedIdx(0);
  }, [query]);

  // Group results by category
  const grouped = groupByCategory(results);

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="sm:max-w-lg p-0 gap-0 overflow-hidden">
        {/* Search input */}
        <div className="flex items-center border-b border-border px-3">
          <Search className="h-4 w-4 text-muted-foreground shrink-0" />
          <Input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Search issues, pages, agents, or type a command…"
            className="border-0 focus-visible:ring-0 focus-visible:ring-offset-0"
          />
          <kbd className="text-[10px] text-muted-foreground bg-muted rounded px-1.5 py-0.5 font-mono shrink-0">
            ESC
          </kbd>
        </div>

        {/* Results */}
        <div ref={listRef} className="max-h-80 overflow-y-auto py-1">
          {results.length === 0 ? (
            <p className="px-4 py-8 text-center text-sm text-muted-foreground">
              No results found.
            </p>
          ) : (
            grouped.map(({ category, items }) => (
              <div key={category}>
                <p className="px-3 pt-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  {categoryLabel(category)}
                </p>
                {items.map(({ item, globalIdx }) => (
                  <button
                    key={item.id}
                    type="button"
                    data-idx={globalIdx}
                    className={`flex w-full items-center gap-3 px-3 py-2 text-sm text-left transition-colors ${
                      globalIdx === clampedIdx
                        ? "bg-accent text-accent-foreground"
                        : "text-foreground hover:bg-accent/50"
                    }`}
                    onClick={() => selectItem(item)}
                    onMouseEnter={() => setSelectedIdx(globalIdx)}
                  >
                    <item.icon className="h-4 w-4 text-muted-foreground shrink-0" />
                    <span className="flex-1 truncate">{item.label}</span>
                    {item.sublabel && (
                      <span className="text-xs text-muted-foreground truncate max-w-[150px]">
                        {item.sublabel}
                      </span>
                    )}
                  </button>
                ))}
              </div>
            ))
          )}
        </div>

        {/* Footer hint */}
        <div className="border-t border-border px-3 py-2 flex items-center gap-3 text-[10px] text-muted-foreground">
          <span>
            <kbd className="bg-muted rounded px-1 py-0.5 font-mono">↑↓</kbd> navigate
          </span>
          <span>
            <kbd className="bg-muted rounded px-1 py-0.5 font-mono">↵</kbd> select
          </span>
          <span>
            <kbd className="bg-muted rounded px-1 py-0.5 font-mono">esc</kbd> close
          </span>
        </div>
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

type Category = PaletteItem["category"];

interface GroupedItem {
  item: PaletteItem;
  globalIdx: number;
}

interface Group {
  category: Category;
  items: GroupedItem[];
}

function groupByCategory(items: PaletteItem[]): Group[] {
  const order: Category[] = ["action", "issue", "knowledge", "agent", "navigate"];
  const map = new Map<Category, GroupedItem[]>();

  items.forEach((item, idx) => {
    const list = map.get(item.category) ?? [];
    list.push({ item, globalIdx: idx });
    map.set(item.category, list);
  });

  return order
    .filter((cat) => map.has(cat))
    .map((cat) => ({ category: cat, items: map.get(cat)! }));
}

function categoryLabel(cat: Category): string {
  switch (cat) {
    case "navigate": return "Pages";
    case "issue": return "Issues";
    case "knowledge": return "Knowledge";
    case "agent": return "Agents";
    case "action": return "Actions";
  }
}
