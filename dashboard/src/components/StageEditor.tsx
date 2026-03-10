import { useState, useCallback } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import {
  ChevronDown,
  ChevronRight,
  GripVertical,
  Plus,
  Trash2,
  Link2,
  Unlink,
  Clock,
  Users,
  Pencil,
} from "lucide-react";
import type { OrchestratorPlan, OrchestratorStage } from "@/lib/types";

interface StageEditorProps {
  plan: OrchestratorPlan;
  onUpdatePhase: (phaseId: string, patch: { title?: string; description?: string }) => void;
  onAddStage: (phaseId: string, stage: OrchestratorStage) => void;
  onRemoveStage: (phaseId: string, stageId: string) => void;
  onUpdateStage: (
    phaseId: string,
    stageId: string,
    patch: Partial<Pick<OrchestratorStage, "title" | "description" | "agent_count" | "complexity_hours">>,
  ) => void;
  onAddDependency: (phaseId: string, stageId: string, dependsOnStageId: string) => void;
  onRemoveDependency: (phaseId: string, stageId: string, dependsOnStageId: string) => void;
  onReorderStages: (phaseId: string, fromIndex: number, toIndex: number) => void;
  readOnly?: boolean;
}

const STATUS_VARIANT: Record<string, "success" | "info" | "warning" | "destructive" | "secondary"> = {
  done: "success",
  running: "info",
  blocked: "warning",
  failed: "destructive",
  pending: "secondary",
  skipped: "secondary",
};

function generateStageId(): string {
  return `stage-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

/** Collect all stage IDs across the entire plan for dependency picking. */
function allStageIds(plan: OrchestratorPlan): { id: string; title: string; phaseTitle: string }[] {
  const result: { id: string; title: string; phaseTitle: string }[] = [];
  for (const phase of plan.phases) {
    for (const stage of phase.stages) {
      result.push({ id: stage.id, title: stage.title, phaseTitle: phase.title });
    }
  }
  return result;
}

// ── Inline stage row ────────────────────────────────────────────────────────

interface StageRowProps {
  stage: OrchestratorStage;
  phaseId: string;
  index: number;
  plan: OrchestratorPlan;
  onUpdate: StageEditorProps["onUpdateStage"];
  onRemove: StageEditorProps["onRemoveStage"];
  onAddDep: StageEditorProps["onAddDependency"];
  onRemoveDep: StageEditorProps["onRemoveDependency"];
  onReorder: StageEditorProps["onReorderStages"];
  stageCount: number;
  readOnly: boolean;
}

function StageRow({
  stage,
  phaseId,
  index,
  plan,
  onUpdate,
  onRemove,
  onAddDep,
  onRemoveDep,
  onReorder,
  stageCount,
  readOnly,
}: StageRowProps) {
  const [expanded, setExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(stage.title);
  const [description, setDescription] = useState(stage.description);
  const [complexity, setComplexity] = useState(String(stage.complexity_hours));
  const [agentCount, setAgentCount] = useState(String(stage.agent_count));
  const [depPickerOpen, setDepPickerOpen] = useState(false);

  const commitEdit = useCallback(() => {
    const patch: Partial<Pick<OrchestratorStage, "title" | "description" | "agent_count" | "complexity_hours">> = {};
    if (title !== stage.title) patch.title = title;
    if (description !== stage.description) patch.description = description;
    const hrs = parseFloat(complexity);
    if (!isNaN(hrs) && hrs !== stage.complexity_hours) patch.complexity_hours = hrs;
    const agents = parseInt(agentCount, 10);
    if (!isNaN(agents) && agents !== stage.agent_count) patch.agent_count = agents;
    if (Object.keys(patch).length > 0) {
      onUpdate(phaseId, stage.id, patch);
    }
    setEditing(false);
  }, [title, description, complexity, agentCount, stage, phaseId, onUpdate]);

  const cancelEdit = () => {
    setTitle(stage.title);
    setDescription(stage.description);
    setComplexity(String(stage.complexity_hours));
    setAgentCount(String(stage.agent_count));
    setEditing(false);
  };

  const availableDeps = allStageIds(plan).filter(
    (s) => s.id !== stage.id && !stage.depends_on.includes(s.id),
  );

  const statusVariant = STATUS_VARIANT[stage.status ?? "pending"] ?? "secondary";

  return (
    <div className="rounded-md border border-border">
      {/* Summary row */}
      <div className="flex items-center gap-2 px-3 py-2 text-sm">
        {!readOnly && (
          <div className="flex flex-col gap-0.5 shrink-0">
            <button
              type="button"
              disabled={index === 0}
              onClick={() => onReorder(phaseId, index, index - 1)}
              className="text-muted-foreground hover:text-foreground disabled:opacity-30 p-0"
              title="Move up"
            >
              <GripVertical className="h-3 w-3" />
            </button>
            <button
              type="button"
              disabled={index === stageCount - 1}
              onClick={() => onReorder(phaseId, index, index + 1)}
              className="text-muted-foreground hover:text-foreground disabled:opacity-30 p-0"
              title="Move down"
            >
              <GripVertical className="h-3 w-3" />
            </button>
          </div>
        )}
        <button
          type="button"
          onClick={() => setExpanded(!expanded)}
          className="text-muted-foreground hover:text-foreground shrink-0"
        >
          {expanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        </button>
        <div className="flex-1 min-w-0">
          <p className="truncate font-medium">{stage.title}</p>
          {stage.depends_on.length > 0 && (
            <p className="text-xs text-muted-foreground truncate">
              Depends on: {stage.depends_on.join(", ")}
            </p>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <span className="text-xs text-muted-foreground flex items-center gap-1" title="Estimated hours">
            <Clock className="h-3 w-3" />
            {stage.complexity_hours}h
          </span>
          <span className="text-xs text-muted-foreground flex items-center gap-1" title="Agent count">
            <Users className="h-3 w-3" />
            {stage.agent_count}
          </span>
          {stage.status && (
            <Badge variant={statusVariant}>{stage.status}</Badge>
          )}
          {!readOnly && (
            <button
              type="button"
              onClick={() => {
                setExpanded(true);
                setEditing(true);
              }}
              className="text-muted-foreground hover:text-foreground p-1 rounded"
              title="Edit stage"
            >
              <Pencil className="h-3 w-3" />
            </button>
          )}
        </div>
      </div>

      {/* Expanded detail / edit form */}
      {expanded && (
        <div className="border-t border-border px-3 py-3 space-y-3 bg-muted/10">
          {editing && !readOnly ? (
            <>
              <div className="space-y-1">
                <label className="text-xs font-medium text-muted-foreground">Title</label>
                <Input
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  className="h-8 text-sm"
                />
              </div>
              <div className="space-y-1">
                <label className="text-xs font-medium text-muted-foreground">Description</label>
                <textarea
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                  rows={3}
                  className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring resize-none"
                />
              </div>
              <div className="flex gap-3">
                <div className="space-y-1 flex-1">
                  <label className="text-xs font-medium text-muted-foreground">Complexity (hours)</label>
                  <Input
                    type="number"
                    min="0"
                    step="0.5"
                    value={complexity}
                    onChange={(e) => setComplexity(e.target.value)}
                    className="h-8 text-sm"
                  />
                </div>
                <div className="space-y-1 flex-1">
                  <label className="text-xs font-medium text-muted-foreground">Agent count</label>
                  <Input
                    type="number"
                    min="1"
                    step="1"
                    value={agentCount}
                    onChange={(e) => setAgentCount(e.target.value)}
                    className="h-8 text-sm"
                  />
                </div>
              </div>
              <div className="flex gap-2 justify-end">
                <Button size="sm" variant="ghost" onClick={cancelEdit}>
                  Cancel
                </Button>
                <Button size="sm" onClick={commitEdit}>
                  Save
                </Button>
              </div>
            </>
          ) : (
            <>
              {stage.description && (
                <p className="text-sm text-muted-foreground whitespace-pre-wrap">{stage.description}</p>
              )}
              {stage.tasks.length > 0 && (
                <div className="space-y-1">
                  <p className="text-xs font-medium text-muted-foreground">Tasks</p>
                  <ul className="text-xs text-muted-foreground space-y-0.5 list-disc list-inside">
                    {stage.tasks.map((t) => (
                      <li key={t.id}>{t.title}</li>
                    ))}
                  </ul>
                </div>
              )}
            </>
          )}

          {/* Dependencies section */}
          <div className="space-y-1.5">
            <p className="text-xs font-medium text-muted-foreground">Dependencies</p>
            {stage.depends_on.length > 0 ? (
              <div className="flex flex-wrap gap-1.5">
                {stage.depends_on.map((depId) => (
                  <Badge key={depId} variant="outline" className="gap-1 pr-1">
                    <span className="font-mono text-[10px]">{depId}</span>
                    {!readOnly && (
                      <button
                        type="button"
                        onClick={() => onRemoveDep(phaseId, stage.id, depId)}
                        className="text-muted-foreground hover:text-destructive ml-0.5"
                        title="Remove dependency"
                      >
                        <Unlink className="h-3 w-3" />
                      </button>
                    )}
                  </Badge>
                ))}
              </div>
            ) : (
              <p className="text-xs text-muted-foreground/60">No dependencies</p>
            )}
            {!readOnly && (
              <div className="relative">
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 text-xs"
                  onClick={() => setDepPickerOpen(!depPickerOpen)}
                  disabled={availableDeps.length === 0}
                >
                  <Link2 className="h-3 w-3 mr-1" />
                  Add dependency
                </Button>
                {depPickerOpen && availableDeps.length > 0 && (
                  <div className="absolute z-10 mt-1 w-72 max-h-48 overflow-y-auto rounded-md border border-border bg-popover shadow-md">
                    {availableDeps.map((s) => (
                      <button
                        key={s.id}
                        type="button"
                        onClick={() => {
                          onAddDep(phaseId, stage.id, s.id);
                          setDepPickerOpen(false);
                        }}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs hover:bg-accent text-left"
                      >
                        <span className="font-mono text-muted-foreground">{s.id}</span>
                        <span className="truncate">{s.title}</span>
                        <span className="text-muted-foreground/60 shrink-0">({s.phaseTitle})</span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>

          {/* Remove button */}
          {!readOnly && (
            <div className="flex justify-end pt-1">
              <Button
                size="sm"
                variant="ghost"
                className="text-destructive hover:text-destructive hover:bg-destructive/10 h-7 text-xs"
                onClick={() => onRemove(phaseId, stage.id)}
              >
                <Trash2 className="h-3 w-3 mr-1" />
                Remove stage
              </Button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ── New stage form ──────────────────────────────────────────────────────────

interface AddStageFormProps {
  phaseId: string;
  onAdd: (phaseId: string, stage: OrchestratorStage) => void;
  onCancel: () => void;
}

function AddStageForm({ phaseId, onAdd, onCancel }: AddStageFormProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [complexity, setComplexity] = useState("2");
  const [agentCount, setAgentCount] = useState("1");

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!title.trim()) return;
    onAdd(phaseId, {
      id: generateStageId(),
      title: title.trim(),
      description: description.trim(),
      tasks: [],
      depends_on: [],
      agent_count: parseInt(agentCount, 10) || 1,
      complexity_hours: parseFloat(complexity) || 2,
    });
  };

  return (
    <form onSubmit={handleSubmit} className="rounded-md border border-dashed border-border p-3 space-y-2 bg-muted/10">
      <Input
        placeholder="Stage title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        className="h-8 text-sm"
        autoFocus
      />
      <textarea
        placeholder="Description (optional)"
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        rows={2}
        className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring resize-none"
      />
      <div className="flex gap-3">
        <div className="flex-1">
          <Input
            type="number"
            min="0"
            step="0.5"
            placeholder="Hours"
            value={complexity}
            onChange={(e) => setComplexity(e.target.value)}
            className="h-8 text-sm"
          />
        </div>
        <div className="flex-1">
          <Input
            type="number"
            min="1"
            step="1"
            placeholder="Agents"
            value={agentCount}
            onChange={(e) => setAgentCount(e.target.value)}
            className="h-8 text-sm"
          />
        </div>
      </div>
      <div className="flex gap-2 justify-end">
        <Button type="button" size="sm" variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" size="sm" disabled={!title.trim()}>
          Add Stage
        </Button>
      </div>
    </form>
  );
}

// ── Main StageEditor ────────────────────────────────────────────────────────

export function StageEditor({
  plan,
  onUpdatePhase,
  onAddStage,
  onRemoveStage,
  onUpdateStage,
  onAddDependency,
  onRemoveDependency,
  onReorderStages,
  readOnly = false,
}: StageEditorProps) {
  const [expandedPhases, setExpandedPhases] = useState<Set<string>>(
    () => new Set(plan.phases.map((p) => p.id)),
  );
  const [addingStageForPhase, setAddingStageForPhase] = useState<string | null>(null);
  const [editingPhaseId, setEditingPhaseId] = useState<string | null>(null);
  const [phaseTitle, setPhaseTitle] = useState("");
  const [phaseDesc, setPhaseDesc] = useState("");

  const togglePhase = (phaseId: string) => {
    setExpandedPhases((prev) => {
      const next = new Set(prev);
      if (next.has(phaseId)) next.delete(phaseId);
      else next.add(phaseId);
      return next;
    });
  };

  const startEditPhase = (phaseId: string, currentTitle: string, currentDesc: string) => {
    setEditingPhaseId(phaseId);
    setPhaseTitle(currentTitle);
    setPhaseDesc(currentDesc);
  };

  const commitPhaseEdit = () => {
    if (!editingPhaseId) return;
    onUpdatePhase(editingPhaseId, {
      title: phaseTitle,
      description: phaseDesc,
    });
    setEditingPhaseId(null);
  };

  return (
    <div className="space-y-3">
      {/* Plan header */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">{plan.title ?? "Execution Plan"}</h2>
          <p className="text-xs text-muted-foreground">
            {plan.phases.length} phase{plan.phases.length !== 1 ? "s" : ""},{" "}
            {plan.total_stages} stage{plan.total_stages !== 1 ? "s" : ""},{" "}
            ~{plan.estimated_hours}h estimated
          </p>
        </div>
      </div>

      {/* Phase accordions */}
      {plan.phases.map((phase) => {
        const isExpanded = expandedPhases.has(phase.id);
        const doneCount = phase.stages.filter((s) => s.status === "done").length;
        const totalCount = phase.stages.length;

        return (
          <Card key={phase.id}>
            <CardHeader
              className="cursor-pointer select-none pb-2"
              onClick={() => togglePhase(phase.id)}
            >
              <div className="flex items-center gap-2">
                {isExpanded ? (
                  <ChevronDown className="h-4 w-4 text-muted-foreground shrink-0" />
                ) : (
                  <ChevronRight className="h-4 w-4 text-muted-foreground shrink-0" />
                )}
                <div className="flex-1 min-w-0">
                  {editingPhaseId === phase.id ? (
                    <div
                      className="space-y-2"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <Input
                        value={phaseTitle}
                        onChange={(e) => setPhaseTitle(e.target.value)}
                        className="h-7 text-sm font-semibold"
                        autoFocus
                      />
                      <textarea
                        value={phaseDesc}
                        onChange={(e) => setPhaseDesc(e.target.value)}
                        rows={2}
                        className="flex w-full rounded-md border border-input bg-background px-3 py-1 text-xs ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring resize-none"
                        placeholder="Phase description"
                      />
                      <div className="flex gap-2">
                        <Button
                          size="sm"
                          variant="ghost"
                          className="h-6 text-xs"
                          onClick={() => setEditingPhaseId(null)}
                        >
                          Cancel
                        </Button>
                        <Button size="sm" className="h-6 text-xs" onClick={commitPhaseEdit}>
                          Save
                        </Button>
                      </div>
                    </div>
                  ) : (
                    <>
                      <CardTitle className="text-sm flex items-center gap-2">
                        {phase.title}
                        {!readOnly && (
                          <button
                            type="button"
                            onClick={(e) => {
                              e.stopPropagation();
                              startEditPhase(phase.id, phase.title, phase.description);
                            }}
                            className="text-muted-foreground hover:text-foreground"
                          >
                            <Pencil className="h-3 w-3" />
                          </button>
                        )}
                      </CardTitle>
                      {phase.description && (
                        <p className="text-xs text-muted-foreground">{phase.description}</p>
                      )}
                    </>
                  )}
                </div>
                <div className="flex items-center gap-2 shrink-0">
                  <Badge variant="outline" className="text-[10px]">
                    {doneCount}/{totalCount}
                  </Badge>
                  {phase.gate_criteria.length > 0 && (
                    <span className="text-[10px] text-muted-foreground/60" title={phase.gate_criteria.join(", ")}>
                      {phase.gate_criteria.length} gate{phase.gate_criteria.length !== 1 ? "s" : ""}
                    </span>
                  )}
                </div>
              </div>
            </CardHeader>

            {isExpanded && (
              <CardContent className="space-y-2 pt-0">
                {phase.stages.map((stage, idx) => (
                  <StageRow
                    key={stage.id}
                    stage={stage}
                    phaseId={phase.id}
                    index={idx}
                    plan={plan}
                    onUpdate={onUpdateStage}
                    onRemove={onRemoveStage}
                    onAddDep={onAddDependency}
                    onRemoveDep={onRemoveDependency}
                    onReorder={onReorderStages}
                    stageCount={phase.stages.length}
                    readOnly={readOnly}
                  />
                ))}

                {/* Add stage */}
                {!readOnly && (
                  addingStageForPhase === phase.id ? (
                    <AddStageForm
                      phaseId={phase.id}
                      onAdd={(pid, s) => {
                        onAddStage(pid, s);
                        setAddingStageForPhase(null);
                      }}
                      onCancel={() => setAddingStageForPhase(null)}
                    />
                  ) : (
                    <Button
                      size="sm"
                      variant="ghost"
                      className="w-full border border-dashed border-border text-muted-foreground hover:text-foreground h-8 text-xs"
                      onClick={() => setAddingStageForPhase(phase.id)}
                    >
                      <Plus className="h-3 w-3 mr-1" />
                      Add stage
                    </Button>
                  )
                )}
              </CardContent>
            )}
          </Card>
        );
      })}
    </div>
  );
}
