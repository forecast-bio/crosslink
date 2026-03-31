import { useCallback, useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { Plus, CircleDot, CheckCircle2, Tag, Milestone, CheckSquare, Square } from "lucide-react";
import { useIssuesStore } from "@/stores/issues";
import { issues as issuesApi, milestones as milestonesApi } from "@/api/client";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogFooter } from "@/components/ui/dialog";
import { IssueForm } from "@/components/IssueForm";
import { formatRelativeTime, priorityVariant } from "@/lib/utils";
import type { IssuePriority, MilestoneDetail } from "@/lib/types";

const PRIORITIES: IssuePriority[] = ["critical", "high", "medium", "low"];

// ---------------------------------------------------------------------------
// Bulk action bar
// ---------------------------------------------------------------------------

interface BulkBarProps {
  selectedIds: Set<number>;
  onClear: () => void;
  onClose: () => void;
  onLabel: () => void;
  onMilestone: () => void;
  busy: boolean;
}

function BulkBar({ selectedIds, onClear, onClose, onLabel, onMilestone, busy }: BulkBarProps) {
  if (selectedIds.size === 0) return null;
  return (
    <div className="flex items-center gap-3 rounded-md border border-border bg-accent/40 px-4 py-2 text-sm">
      <span className="font-medium">{selectedIds.size} selected</span>
      <div className="flex gap-1 ml-2">
        <Button size="sm" variant="outline" className="h-7 gap-1" onClick={onClose} disabled={busy}>
          <CheckCircle2 className="h-3 w-3" />
          Close all
        </Button>
        <Button size="sm" variant="outline" className="h-7 gap-1" onClick={onLabel} disabled={busy}>
          <Tag className="h-3 w-3" />
          Add label
        </Button>
        <Button size="sm" variant="outline" className="h-7 gap-1" onClick={onMilestone} disabled={busy}>
          <Milestone className="h-3 w-3" />
          Assign milestone
        </Button>
      </div>
      <button
        type="button"
        className="ml-auto text-xs text-muted-foreground hover:text-foreground"
        onClick={onClear}
      >
        Clear
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function Issues() {
  const { issues, loading, fetch, create } = useIssuesStore();
  const [search, setSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<"open" | "closed" | "all">("open");
  const [priorityFilter, setPriorityFilter] = useState<IssuePriority | "all">("all");
  const [showForm, setShowForm] = useState(false);

  // Bulk selection
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [bulkBusy, setBulkBusy] = useState(false);

  // Bulk label dialog
  const [labelDialogOpen, setLabelDialogOpen] = useState(false);
  const [bulkLabel, setBulkLabel] = useState("");

  // Bulk milestone dialog
  const [milestoneDialogOpen, setMilestoneDialogOpen] = useState(false);
  const [milestones, setMilestones] = useState<MilestoneDetail[]>([]);
  const [milestonesLoading, setMilestonesLoading] = useState(false);

  const refetch = useCallback(() => fetch({
    status: statusFilter === "all" ? undefined : statusFilter,
    priority: priorityFilter === "all" ? undefined : priorityFilter,
  }), [fetch, statusFilter, priorityFilter]);

  useEffect(() => {
    void refetch();
    setSelected(new Set());
  }, [refetch]);

  // Client-side search filter
  const filtered = search
    ? issues.filter(
        (i) =>
          i.title.toLowerCase().includes(search.toLowerCase()) ||
          String(i.id).includes(search),
      )
    : issues;

  const toggleSelect = (id: number, e: React.MouseEvent) => {
    e.preventDefault();
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleSelectAll = () => {
    if (selected.size === filtered.length) {
      setSelected(new Set());
    } else {
      setSelected(new Set(filtered.map((i) => i.id)));
    }
  };

  const handleClose = async (id: number, e: React.MouseEvent) => {
    e.preventDefault();
    await issuesApi.close(id);
    void refetch();
  };

  const handleBulkClose = async () => {
    setBulkBusy(true);
    try {
      await Promise.all([...selected].map((id) => issuesApi.close(id)));
      setSelected(new Set());
      void refetch();
    } finally {
      setBulkBusy(false);
    }
  };

  const openLabelDialog = () => {
    setBulkLabel("");
    setLabelDialogOpen(true);
  };

  const handleBulkLabel = async () => {
    const label = bulkLabel.trim().toLowerCase();
    if (!label) return;
    setBulkBusy(true);
    try {
      await Promise.all([...selected].map((id) => issuesApi.addLabel(id, label)));
      setSelected(new Set());
      setLabelDialogOpen(false);
      void refetch();
    } finally {
      setBulkBusy(false);
    }
  };

  const openMilestoneDialog = async () => {
    setMilestonesLoading(true);
    setMilestoneDialogOpen(true);
    try {
      const data = await milestonesApi.list();
      setMilestones(data.filter((m) => m.status === "open"));
    } finally {
      setMilestonesLoading(false);
    }
  };

  const handleBulkMilestone = async (milestoneId: number) => {
    setBulkBusy(true);
    try {
      await Promise.all(
        [...selected].map((issueId) => milestonesApi.assign(milestoneId, issueId)),
      );
      setSelected(new Set());
      setMilestoneDialogOpen(false);
      void refetch();
    } finally {
      setBulkBusy(false);
    }
  };

  const allSelected = filtered.length > 0 && selected.size === filtered.length;
  const someSelected = selected.size > 0 && !allSelected;

  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Issues</h1>
        <Button size="sm" onClick={() => setShowForm(true)}>
          <Plus className="h-4 w-4 mr-1" /> New Issue
        </Button>
      </div>

      {/* Filters row */}
      <div className="flex flex-wrap items-center gap-3">
        <Input
          placeholder="Search issues…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="max-w-xs"
        />

        {/* Status filter */}
        <div className="flex gap-1">
          {(["open", "closed", "all"] as const).map((s) => (
            <Button
              key={s}
              size="sm"
              variant={statusFilter === s ? "secondary" : "ghost"}
              onClick={() => setStatusFilter(s)}
              className="capitalize"
            >
              {s}
            </Button>
          ))}
        </div>

        {/* Priority filter */}
        <div className="flex gap-1">
          <Button
            size="sm"
            variant={priorityFilter === "all" ? "secondary" : "ghost"}
            onClick={() => setPriorityFilter("all")}
          >
            All priorities
          </Button>
          {PRIORITIES.map((p) => (
            <Button
              key={p}
              size="sm"
              variant={priorityFilter === p ? "secondary" : "ghost"}
              onClick={() => setPriorityFilter(p)}
              className="capitalize"
            >
              {p}
            </Button>
          ))}
        </div>
      </div>

      {/* Bulk action bar */}
      <BulkBar
        selectedIds={selected}
        onClear={() => setSelected(new Set())}
        onClose={() => void handleBulkClose()}
        onLabel={openLabelDialog}
        onMilestone={() => void openMilestoneDialog()}
        busy={bulkBusy}
      />

      {loading ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : filtered.length === 0 ? (
        <Card>
          <CardContent className="py-10 text-center text-muted-foreground text-sm">
            No issues found.
          </CardContent>
        </Card>
      ) : (
        <div className="space-y-1">
          {/* Select-all header */}
          <div className="flex items-center gap-2 px-4 py-1">
            <SelectCheckbox
              checked={allSelected}
              indeterminate={someSelected}
              onChange={toggleSelectAll}
              aria-label="Select all"
            />
            <span className="text-xs text-muted-foreground">
              {filtered.length} issue{filtered.length !== 1 ? "s" : ""}
            </span>
          </div>

          {filtered.map((issue) => (
            <div
              key={issue.id}
              className="flex items-center gap-2 rounded-md border border-border bg-card hover:bg-accent/30 transition-colors"
            >
              {/* Checkbox */}
              <div
                className="pl-4 py-3 shrink-0"
                onClick={(e) => toggleSelect(issue.id, e)}
              >
                <SelectCheckbox
                  checked={selected.has(issue.id)}
                  onChange={() => {}}
                  aria-label={`Select issue #${issue.id}`}
                />
              </div>

              {/* Issue row */}
              <Link
                to={`/issues/${issue.id}`}
                className="flex flex-1 items-center gap-3 py-3 pr-4 min-w-0"
              >
                {issue.status === "open" ? (
                  <CircleDot className="h-4 w-4 text-green-400 shrink-0" />
                ) : (
                  <CheckCircle2 className="h-4 w-4 text-muted-foreground shrink-0" />
                )}
                <span className="font-mono text-xs text-muted-foreground w-8 shrink-0">
                  #{issue.id}
                </span>
                <span className="flex-1 text-sm truncate">{issue.title}</span>
                <Badge variant={priorityVariant(issue.priority)} className="shrink-0">
                  {issue.priority}
                </Badge>
                <span className="text-xs text-muted-foreground shrink-0">
                  {formatRelativeTime(issue.updated_at)}
                </span>
                {issue.status === "open" && (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-6 px-2 text-xs shrink-0"
                    onClick={(e) => void handleClose(issue.id, e)}
                  >
                    Close
                  </Button>
                )}
              </Link>
            </div>
          ))}
        </div>
      )}

      {/* New issue form dialog */}
      <IssueForm
        open={showForm}
        onOpenChange={setShowForm}
        onSubmit={async (data) => {
          await create(data);
        }}
      />

      {/* Bulk label dialog */}
      <Dialog open={labelDialogOpen} onOpenChange={setLabelDialogOpen}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>Add label to {selected.size} issue{selected.size !== 1 ? "s" : ""}</DialogTitle>
          </DialogHeader>
          <div className="py-2">
            <Input
              placeholder="Label name…"
              value={bulkLabel}
              onChange={(e) => setBulkLabel(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void handleBulkLabel();
              }}
              autoFocus
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setLabelDialogOpen(false)}>
              Cancel
            </Button>
            <Button
              disabled={!bulkLabel.trim() || bulkBusy}
              onClick={() => void handleBulkLabel()}
            >
              Add label
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Bulk milestone dialog */}
      <Dialog open={milestoneDialogOpen} onOpenChange={setMilestoneDialogOpen}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>
              Assign milestone to {selected.size} issue{selected.size !== 1 ? "s" : ""}
            </DialogTitle>
          </DialogHeader>
          <div className="py-2 space-y-1">
            {milestonesLoading ? (
              <p className="text-sm text-muted-foreground">Loading milestones…</p>
            ) : milestones.length === 0 ? (
              <p className="text-sm text-muted-foreground">No open milestones found.</p>
            ) : (
              milestones.map((m) => (
                <button
                  key={m.id}
                  type="button"
                  className="flex w-full items-center gap-3 rounded-md px-3 py-2 text-sm hover:bg-accent transition-colors text-left"
                  disabled={bulkBusy}
                  onClick={() => void handleBulkMilestone(m.id)}
                >
                  <Milestone className="h-4 w-4 text-muted-foreground shrink-0" />
                  <span className="flex-1">{m.name}</span>
                  <span className="text-xs text-muted-foreground">
                    {m.completed_count}/{m.issue_count}
                  </span>
                </button>
              ))
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setMilestoneDialogOpen(false)}>
              Cancel
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

// ---------------------------------------------------------------------------
// SelectCheckbox
// ---------------------------------------------------------------------------

interface SelectCheckboxProps {
  checked: boolean;
  indeterminate?: boolean;
  onChange: (checked: boolean) => void;
  "aria-label"?: string;
}

function SelectCheckbox({ checked, indeterminate, onChange, "aria-label": ariaLabel }: SelectCheckboxProps) {
  const ref = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (ref.current) {
      ref.current.setAttribute("aria-checked", indeterminate ? "mixed" : String(checked));
    }
  }, [checked, indeterminate]);

  return (
    <button
      ref={ref}
      type="button"
      role="checkbox"
      aria-label={ariaLabel}
      aria-checked={indeterminate ? "mixed" : checked}
      className="h-4 w-4 shrink-0 text-muted-foreground hover:text-foreground transition-colors"
      onClick={() => onChange(!checked)}
    >
      {checked || indeterminate ? (
        <CheckSquare className="h-4 w-4 text-blue-400" />
      ) : (
        <Square className="h-4 w-4" />
      )}
    </button>
  );
}
