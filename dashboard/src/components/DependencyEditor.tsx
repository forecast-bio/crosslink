import { useEffect, useRef, useState } from "react";
import { X, Plus, Link as LinkIcon, Search } from "lucide-react";
import { issues as issuesApi } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import type { Issue } from "@/lib/types";

interface DependencyEditorProps {
  issueId: number;
  /** IDs of issues that block this issue. */
  blockers: number[];
  /** IDs of issues that this issue blocks. */
  blocking: number[];
  /** Called after a dependency is added or removed so the parent can refresh. */
  onChange: () => void;
}

/**
 * Inline dependency management widget.
 * Shows "Blocked by" and "Blocking" lists, and provides an issue-picker
 * input to add new blockers.
 */
export function DependencyEditor({ issueId, blockers, blocking, onChange }: DependencyEditorProps) {
  const [input, setInput] = useState("");
  const [suggestions, setSuggestions] = useState<Issue[]>([]);
  const [showSuggestions, setShowSuggestions] = useState(false);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const searchTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Close dropdown on outside click
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setShowSuggestions(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  const searchIssues = (query: string) => {
    if (searchTimeout.current) clearTimeout(searchTimeout.current);
    if (!query.trim()) {
      setSuggestions([]);
      setShowSuggestions(false);
      return;
    }
    searchTimeout.current = setTimeout(async () => {
      try {
        // Try numeric ID first
        const numId = parseInt(query, 10);
        if (!isNaN(numId)) {
          const issue = await issuesApi.get(numId).catch(() => null);
          if (issue && issue.id !== issueId && !blockers.includes(issue.id)) {
            setSuggestions([issue]);
            setShowSuggestions(true);
            return;
          }
        }
        // Fall back to text search
        const results = await issuesApi.list({ search: query, status: "open" });
        const filtered = results.filter(
          (i) => i.id !== issueId && !blockers.includes(i.id),
        );
        setSuggestions(filtered.slice(0, 6));
        setShowSuggestions(filtered.length > 0);
      } catch {
        setSuggestions([]);
      }
    }, 200);
  };

  const addBlocker = async (blockerId: number) => {
    if (blockers.includes(blockerId)) return;
    setBusy(true);
    try {
      await issuesApi.addBlocker(issueId, blockerId);
      setInput("");
      setSuggestions([]);
      setShowSuggestions(false);
      onChange();
    } finally {
      setBusy(false);
    }
  };

  const removeBlocker = async (blockerId: number) => {
    setBusy(true);
    try {
      await issuesApi.removeBlocker(issueId, blockerId);
      onChange();
    } finally {
      setBusy(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      const numId = parseInt(input.trim(), 10);
      if (!isNaN(numId)) void addBlocker(numId);
    } else if (e.key === "Escape") {
      setInput("");
      setSuggestions([]);
      setShowSuggestions(false);
    }
  };

  return (
    <div className="space-y-3">
      {/* Blocked by */}
      <div className="space-y-1">
        <p className="text-xs font-medium text-muted-foreground">Blocked by</p>
        {blockers.length === 0 ? (
          <p className="text-xs text-muted-foreground/60">None</p>
        ) : (
          <div className="flex flex-wrap gap-1">
            {blockers.map((bid) => (
              <IssueChip
                key={bid}
                issueId={bid}
                onRemove={() => void removeBlocker(bid)}
                disabled={busy}
              />
            ))}
          </div>
        )}
      </div>

      {/* Blocking */}
      {blocking.length > 0 && (
        <div className="space-y-1">
          <p className="text-xs font-medium text-muted-foreground">Blocking</p>
          <div className="flex flex-wrap gap-1">
            {blocking.map((bid) => (
              <IssueChip key={bid} issueId={bid} disabled />
            ))}
          </div>
        </div>
      )}

      {/* Add blocker input */}
      <div ref={containerRef} className="relative">
        <div className="flex gap-1">
          <div className="relative flex-1">
            <Search className="pointer-events-none absolute left-2 top-1/2 h-3 w-3 -translate-y-1/2 text-muted-foreground" />
            <Input
              ref={inputRef}
              placeholder="Issue # or title…"
              value={input}
              onChange={(e) => {
                setInput(e.target.value);
                searchIssues(e.target.value);
              }}
              onKeyDown={handleKeyDown}
              disabled={busy}
              className="h-7 pl-7 text-xs"
            />
          </div>
          <Button
            size="sm"
            variant="outline"
            className="h-7 px-2"
            disabled={busy || !input.trim() || isNaN(parseInt(input.trim(), 10))}
            onClick={() => {
              const numId = parseInt(input.trim(), 10);
              if (!isNaN(numId)) void addBlocker(numId);
            }}
          >
            <Plus className="h-3 w-3" />
          </Button>
        </div>

        {/* Issue picker dropdown */}
        {showSuggestions && suggestions.length > 0 && (
          <div className="absolute z-50 mt-1 w-full rounded-md border border-border bg-popover shadow-md">
            {suggestions.map((s) => (
              <button
                key={s.id}
                type="button"
                className={cn(
                  "flex w-full items-center gap-2 px-3 py-1.5 text-xs hover:bg-accent transition-colors",
                )}
                onMouseDown={(e) => {
                  e.preventDefault();
                  void addBlocker(s.id);
                }}
              >
                <LinkIcon className="h-3 w-3 shrink-0 text-muted-foreground" />
                <span className="font-mono text-muted-foreground shrink-0">#{s.id}</span>
                <span className="truncate">{s.title}</span>
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// IssueChip — small chip showing an issue ID with optional remove button
// ---------------------------------------------------------------------------

interface IssueChipProps {
  issueId: number;
  onRemove?: () => void;
  disabled?: boolean;
}

function IssueChip({ issueId, onRemove, disabled }: IssueChipProps) {
  return (
    <span className="inline-flex items-center gap-1 rounded-full border border-border bg-secondary/50 px-2 py-0.5 text-xs font-mono">
      #{issueId}
      {onRemove && (
        <button
          type="button"
          className="ml-0.5 rounded-full hover:text-destructive transition-colors disabled:opacity-40"
          disabled={disabled}
          onClick={onRemove}
          aria-label={`Remove blocker #${issueId}`}
        >
          <X className="h-3 w-3" />
        </button>
      )}
    </span>
  );
}
