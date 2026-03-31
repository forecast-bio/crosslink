import { useEffect, useState } from "react";
import { Link } from "react-router";
import { Search, CircleDot, CheckCircle2 } from "lucide-react";
import { sessions as sessionsApi, issues as issuesApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { formatDateTime, formatRelativeTime } from "@/lib/utils";
import type { Session, Issue } from "@/lib/types";

export function Sessions() {
  const [current, setCurrent] = useState<Session | null>(null);
  const [loading, setLoading] = useState(true);
  const [endNotes, setEndNotes] = useState("");

  // Issue picker state
  const [issueSearch, setIssueSearch] = useState("");
  const [issueResults, setIssueResults] = useState<Issue[]>([]);
  const [issueSearching, setIssueSearching] = useState(false);
  const [workingOn, setWorkingOn] = useState<Issue | null>(null);

  const refresh = () => {
    setLoading(true);
    sessionsApi
      .current()
      .then(setCurrent)
      .catch(() => setCurrent(null))
      .finally(() => setLoading(false));
  };

  useEffect(() => { refresh(); }, []);

  // Load the active issue when session changes
  useEffect(() => {
    if (current?.active_issue_id) {
      issuesApi
        .get(current.active_issue_id)
        .then(setWorkingOn)
        .catch(() => setWorkingOn(null));
    } else {
      setWorkingOn(null);
    }
  }, [current?.active_issue_id]);

  const handleStart = async () => {
    setLoading(true);
    await sessionsApi.start();
    refresh();
  };

  const handleEnd = async () => {
    setLoading(true);
    await sessionsApi.end(endNotes.trim() || undefined);
    setEndNotes("");
    refresh();
  };

  const handleWorkOn = async (issue: Issue) => {
    await sessionsApi.work(issue.id);
    setWorkingOn(issue);
    setCurrent((prev) => prev ? { ...prev, active_issue_id: issue.id } : prev);
    setIssueSearch("");
    setIssueResults([]);
  };

  const handleClearWork = async () => {
    await sessionsApi.clearWork().catch(() => null);
    setWorkingOn(null);
    setCurrent((prev) => prev ? { ...prev, active_issue_id: null } : prev);
  };

  // Debounced issue search
  useEffect(() => {
    if (!issueSearch.trim()) {
      setIssueResults([]);
      return;
    }
    setIssueSearching(true);
    const timer = setTimeout(async () => {
      try {
        const results = await issuesApi.list({ search: issueSearch, status: "open" });
        setIssueResults(results.slice(0, 8));
      } catch {
        setIssueResults([]);
      } finally {
        setIssueSearching(false);
      }
    }, 250);
    return () => clearTimeout(timer);
  }, [issueSearch]);

  return (
    <div className="p-6 space-y-4 max-w-xl">
      <h1 className="text-2xl font-bold">Sessions</h1>

      {/* ── Current session card ─────────────────────────────────────────── */}
      <Card>
        <CardHeader>
          <CardTitle className="text-sm flex items-center justify-between">
            Current Session
            {current ? (
              <Badge variant="success">active</Badge>
            ) : (
              <Badge variant="secondary">none</Badge>
            )}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 text-sm">
          {loading ? (
            <p className="text-muted-foreground">Loading…</p>
          ) : current ? (
            <>
              {/* Session metadata */}
              <div className="flex justify-between">
                <span className="text-muted-foreground">Agent</span>
                <span className="font-mono text-xs">{current.agent_id ?? "—"}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Started</span>
                <span>{formatRelativeTime(current.started_at)}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Exact</span>
                <span className="text-xs text-muted-foreground">{formatDateTime(current.started_at)}</span>
              </div>

              {/* Working-on display */}
              <div className="border-t border-border pt-3 space-y-2">
                <p className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
                  Working on
                </p>
                {workingOn ? (
                  <div className="flex items-start gap-2 rounded-md border border-border bg-secondary/30 px-3 py-2">
                    <CircleDot className="h-4 w-4 text-green-400 mt-0.5 shrink-0" />
                    <div className="flex-1 min-w-0">
                      <Link
                        to={`/issues/${workingOn.id}`}
                        className="text-sm text-blue-400 hover:underline truncate block"
                      >
                        #{workingOn.id} {workingOn.title}
                      </Link>
                      <Badge variant="outline" className="mt-1 text-xs">
                        {workingOn.priority}
                      </Badge>
                    </div>
                    <button
                      type="button"
                      className="text-xs text-muted-foreground hover:text-foreground shrink-0"
                      onClick={() => void handleClearWork()}
                    >
                      Clear
                    </button>
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground/60">Not focused on any issue.</p>
                )}

                {/* Issue picker */}
                <IssuePicker
                  value={issueSearch}
                  onChange={setIssueSearch}
                  results={issueResults}
                  searching={issueSearching}
                  onSelect={handleWorkOn}
                  placeholder="Search issues to work on…"
                />
              </div>

              {/* Handoff notes */}
              {current.handoff_notes && (
                <p className="text-xs text-muted-foreground border-t border-border pt-2 whitespace-pre-wrap">
                  {current.handoff_notes}
                </p>
              )}

              {/* End session */}
              <div className="border-t border-border pt-3 space-y-2">
                <Input
                  placeholder="Handoff notes (optional)…"
                  value={endNotes}
                  onChange={(e) => setEndNotes(e.target.value)}
                  className="h-8 text-xs"
                />
                <Button size="sm" variant="outline" onClick={() => void handleEnd()}>
                  End Session
                </Button>
              </div>
            </>
          ) : (
            <>
              <p className="text-muted-foreground">No active session.</p>
              <Button size="sm" onClick={() => void handleStart()}>
                Start Session
              </Button>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// IssuePicker — search-as-you-type issue selector
// ---------------------------------------------------------------------------

interface IssuePickerProps {
  value: string;
  onChange: (v: string) => void;
  results: Issue[];
  searching: boolean;
  onSelect: (issue: Issue) => void;
  placeholder?: string;
}

function IssuePicker({ value, onChange, results, searching, onSelect, placeholder }: IssuePickerProps) {
  return (
    <div className="relative">
      <div className="relative">
        <Search className="pointer-events-none absolute left-2 top-1/2 h-3 w-3 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder ?? "Search issues…"}
          className="h-7 pl-7 text-xs"
        />
      </div>

      {(searching || results.length > 0) && (
        <div className="absolute z-50 mt-1 w-full rounded-md border border-border bg-popover shadow-md">
          {searching && (
            <div className="px-3 py-2 text-xs text-muted-foreground">Searching…</div>
          )}
          {!searching &&
            results.map((issue) => (
              <button
                key={issue.id}
                type="button"
                className="flex w-full items-center gap-2 px-3 py-1.5 text-xs hover:bg-accent transition-colors"
                onMouseDown={(e) => {
                  e.preventDefault();
                  onSelect(issue);
                }}
              >
                {issue.status === "open" ? (
                  <CircleDot className="h-3 w-3 text-green-400 shrink-0" />
                ) : (
                  <CheckCircle2 className="h-3 w-3 text-muted-foreground shrink-0" />
                )}
                <span className="font-mono text-muted-foreground shrink-0">#{issue.id}</span>
                <span className="truncate">{issue.title}</span>
              </button>
            ))}
        </div>
      )}
    </div>
  );
}
