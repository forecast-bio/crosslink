import { useEffect, useRef, useState } from "react";
import { X, Plus, Tag } from "lucide-react";
import { issues as issuesApi } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

// Well-known labels shown as autocomplete suggestions
const COMMON_LABELS = [
  "bug", "fix", "feature", "enhancement", "breaking", "breaking-change",
  "security", "deprecated", "removed", "chore", "docs", "test",
  "high", "medium", "low", "critical", "blocked", "needs-review",
];

interface LabelManagerProps {
  issueId: number;
  labels: string[];
  /** Called after a label is added or removed so the parent can refresh. */
  onChange: () => void;
}

/**
 * Inline label management widget.
 * Shows existing labels as removable chips plus an autocomplete input for
 * adding new ones.
 */
export function LabelManager({ issueId, labels, onChange }: LabelManagerProps) {
  const [input, setInput] = useState("");
  const [showSuggestions, setShowSuggestions] = useState(false);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  const suggestions = COMMON_LABELS.filter(
    (l) =>
      l.toLowerCase().includes(input.toLowerCase()) &&
      !labels.includes(l),
  ).slice(0, 6);

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

  const addLabel = async (label: string) => {
    const trimmed = label.trim().toLowerCase();
    if (!trimmed || labels.includes(trimmed)) return;
    setBusy(true);
    try {
      await issuesApi.addLabel(issueId, trimmed);
      setInput("");
      setShowSuggestions(false);
      onChange();
    } finally {
      setBusy(false);
    }
  };

  const removeLabel = async (label: string) => {
    setBusy(true);
    try {
      await issuesApi.removeLabel(issueId, label);
      onChange();
    } finally {
      setBusy(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      void addLabel(input);
    } else if (e.key === "Escape") {
      setInput("");
      setShowSuggestions(false);
    }
  };

  return (
    <div ref={containerRef} className="space-y-2">
      {/* Existing label chips */}
      <div className="flex flex-wrap gap-1">
        {labels.length === 0 && (
          <span className="text-xs text-muted-foreground">No labels</span>
        )}
        {labels.map((l) => (
          <span
            key={l}
            className="inline-flex items-center gap-1 rounded-full border border-border bg-secondary/50 px-2 py-0.5 text-xs"
          >
            <Tag className="h-3 w-3 text-muted-foreground" />
            {l}
            <button
              type="button"
              className="ml-0.5 rounded-full hover:text-destructive transition-colors disabled:opacity-40"
              disabled={busy}
              onClick={() => void removeLabel(l)}
              aria-label={`Remove label ${l}`}
            >
              <X className="h-3 w-3" />
            </button>
          </span>
        ))}
      </div>

      {/* Add label input with autocomplete */}
      <div className="relative">
        <div className="flex gap-1">
          <Input
            ref={inputRef}
            placeholder="Add label…"
            value={input}
            onChange={(e) => {
              setInput(e.target.value);
              setShowSuggestions(e.target.value.length > 0);
            }}
            onFocus={() => setShowSuggestions(input.length > 0 || COMMON_LABELS.length > 0)}
            onKeyDown={handleKeyDown}
            disabled={busy}
            className="h-7 text-xs"
          />
          <Button
            size="sm"
            variant="outline"
            className="h-7 px-2"
            disabled={busy || !input.trim()}
            onClick={() => void addLabel(input)}
          >
            <Plus className="h-3 w-3" />
          </Button>
        </div>

        {/* Autocomplete dropdown */}
        {showSuggestions && suggestions.length > 0 && (
          <div className="absolute z-50 mt-1 w-full rounded-md border border-border bg-popover shadow-md">
            {suggestions.map((s) => (
              <button
                key={s}
                type="button"
                className={cn(
                  "flex w-full items-center gap-2 px-3 py-1.5 text-xs hover:bg-accent transition-colors",
                )}
                onMouseDown={(e) => {
                  // Prevent input blur before click registers
                  e.preventDefault();
                  void addLabel(s);
                }}
              >
                <Tag className="h-3 w-3 text-muted-foreground" />
                {s}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
