import { useEffect, useState } from "react";
import { Plus, CheckCircle2, Target } from "lucide-react";
import { milestones as milestonesApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { formatRelativeTime } from "@/lib/utils";
import type { MilestoneDetail } from "@/lib/types";

export function Milestones() {
  const [items, setItems] = useState<MilestoneDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [statusFilter, setStatusFilter] = useState<"open" | "closed" | "all">("open");
  const [showCreate, setShowCreate] = useState(false);

  const refetch = () => {
    milestonesApi
      .list()
      .then(setItems)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    void refetch();
  }, []);

  const filtered =
    statusFilter === "all"
      ? items
      : items.filter((m) => m.status === statusFilter);

  async function handleClose(id: number) {
    await milestonesApi.close(id);
    refetch();
  }

  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Milestones</h1>
        <Button size="sm" onClick={() => setShowCreate(true)}>
          <Plus className="h-4 w-4 mr-1" /> New Milestone
        </Button>
      </div>

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
        <span className="text-xs text-muted-foreground self-center ml-2">
          {filtered.length} milestone{filtered.length !== 1 ? "s" : ""}
        </span>
      </div>

      {error && (
        <p className="text-destructive text-sm">{error}</p>
      )}

      {loading ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : filtered.length === 0 ? (
        <Card>
          <CardContent className="py-10 text-center">
            <Target className="h-8 w-8 text-muted-foreground/40 mx-auto mb-3" />
            <p className="text-muted-foreground text-sm">
              {statusFilter === "all"
                ? "No milestones yet."
                : `No ${statusFilter} milestones.`}
            </p>
          </CardContent>
        </Card>
      ) : (
        <div className="space-y-3">
          {filtered.map((m) => {
            const pct =
              m.issue_count > 0
                ? Math.round((m.completed_count / m.issue_count) * 100)
                : 0;
            return (
              <Card key={m.id}>
                <CardHeader className="pb-2">
                  <CardTitle className="text-base flex items-center justify-between">
                    <div className="flex items-center gap-2 min-w-0">
                      <span className="font-mono text-xs text-muted-foreground shrink-0">
                        M{m.id}
                      </span>
                      <span className="truncate">{m.name}</span>
                    </div>
                    <div className="flex items-center gap-2 shrink-0">
                      <Badge variant={m.status === "open" ? "success" : "secondary"}>
                        {m.status}
                      </Badge>
                      {m.status === "open" && (
                        <Button
                          size="sm"
                          variant="ghost"
                          className="h-7 text-xs"
                          onClick={() => void handleClose(m.id)}
                        >
                          <CheckCircle2 className="h-3 w-3 mr-1" />
                          Close
                        </Button>
                      )}
                    </div>
                  </CardTitle>
                </CardHeader>
                <CardContent className="space-y-2">
                  {m.description && (
                    <p className="text-sm text-muted-foreground">{m.description}</p>
                  )}
                  <div className="flex items-center gap-3 text-xs text-muted-foreground">
                    <span>
                      {m.completed_count}/{m.issue_count} issues closed
                    </span>
                    <span className="font-medium">{pct}%</span>
                    {m.created_at && (
                      <span>Created {formatRelativeTime(m.created_at)}</span>
                    )}
                  </div>
                  <div className="h-2 w-full rounded-full bg-secondary overflow-hidden">
                    <div
                      className={`h-full rounded-full transition-all ${
                        pct === 100
                          ? "bg-green-500"
                          : pct > 50
                            ? "bg-blue-500"
                            : "bg-blue-400"
                      }`}
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                </CardContent>
              </Card>
            );
          })}
        </div>
      )}

      {/* Create milestone dialog */}
      <CreateMilestoneDialog
        open={showCreate}
        onOpenChange={setShowCreate}
        onCreated={refetch}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Create Milestone Dialog
// ---------------------------------------------------------------------------

interface CreateDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: () => void;
}

function CreateMilestoneDialog({ open, onOpenChange, onCreated }: CreateDialogProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [submitting, setSubmitting] = useState(false);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!title.trim()) return;
    setSubmitting(true);
    try {
      await milestonesApi.create({
        title: title.trim(),
        description: description.trim() || undefined,
      });
      setTitle("");
      setDescription("");
      onOpenChange(false);
      onCreated();
    } catch {
      // Error visible in console
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>New Milestone</DialogTitle>
        </DialogHeader>
        <form onSubmit={(e) => void handleSubmit(e)} className="space-y-4">
          <div className="space-y-1">
            <label className="text-sm font-medium" htmlFor="ms-title">
              Name
            </label>
            <Input
              id="ms-title"
              placeholder="Milestone name"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              autoFocus
            />
          </div>
          <div className="space-y-1">
            <label className="text-sm font-medium" htmlFor="ms-desc">
              Description{" "}
              <span className="text-muted-foreground font-normal">(optional)</span>
            </label>
            <textarea
              id="ms-desc"
              rows={3}
              placeholder="What does this milestone track?"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 resize-none"
            />
          </div>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!title.trim() || submitting}>
              {submitting ? "Creating…" : "Create Milestone"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
