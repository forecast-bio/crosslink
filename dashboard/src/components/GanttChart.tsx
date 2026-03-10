import { useMemo } from "react";
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  Cell,
} from "recharts";
import type { OrchestratorPlan, OrchestratorStage, StageStatus } from "@/lib/types";

// ---------------------------------------------------------------------------
// Status → color mapping (consistent with DagGraph)
// ---------------------------------------------------------------------------

const STATUS_BAR_COLORS: Record<StageStatus | "pending", string> = {
  pending: "hsl(220, 13%, 30%)",
  running: "hsl(210, 70%, 55%)",
  done:    "hsl(150, 60%, 45%)",
  failed:  "hsl(0, 70%, 50%)",
  blocked: "hsl(45, 70%, 50%)",
  skipped: "hsl(220, 13%, 25%)",
};

// ---------------------------------------------------------------------------
// Data transformation
// ---------------------------------------------------------------------------

interface GanttRow {
  label: string;
  stageId: string;
  phaseTitle: string;
  status: StageStatus;
  agentId: string | undefined;
  /** Start offset in "stage units" (computed from dependency depth) */
  start: number;
  /** Duration in estimated hours */
  duration: number;
  /** Spacer before the bar (for offset rendering) */
  offset: number;
  complexityHours: number;
}

/**
 * Transform the orchestrator plan into Gantt bar data.
 *
 * Each stage gets a row. The horizontal axis represents sequential time slots
 * derived from dependency depth — stages at depth 0 start immediately,
 * deeper stages start after their last dependency completes.
 */
function planToGanttRows(plan: OrchestratorPlan): GanttRow[] {
  const allStages: (OrchestratorStage & { phaseTitle: string })[] = plan.phases.flatMap(
    (p) => p.stages.map((s) => ({ ...s, phaseTitle: p.title })),
  );

  const stageById = new Map(allStages.map((s) => [s.id, s]));

  // Compute earliest start for each stage (longest path from roots)
  const starts = new Map<string, number>();

  function getStart(id: string, visited: Set<string>): number {
    if (starts.has(id)) return starts.get(id)!;
    if (visited.has(id)) return 0;
    visited.add(id);
    const stage = stageById.get(id);
    if (!stage || stage.depends_on.length === 0) {
      starts.set(id, 0);
      return 0;
    }
    const maxParentEnd = Math.max(
      ...stage.depends_on.map((dep) => {
        const depStage = stageById.get(dep);
        if (!depStage) return 0;
        return getStart(dep, visited) + depStage.complexity_hours;
      }),
    );
    starts.set(id, maxParentEnd);
    return maxParentEnd;
  }

  for (const s of allStages) getStart(s.id, new Set());

  return allStages.map((stage) => {
    const offset = starts.get(stage.id) ?? 0;
    const duration = stage.complexity_hours || 1;
    // Truncate long labels for the Y-axis
    const label =
      stage.title.length > 30 ? stage.title.slice(0, 28) + "…" : stage.title;

    return {
      label,
      stageId: stage.id,
      phaseTitle: stage.phaseTitle,
      status: stage.status ?? "pending",
      agentId: stage.agent_id,
      start: offset,
      duration,
      offset,
      complexityHours: stage.complexity_hours,
    };
  });
}

// ---------------------------------------------------------------------------
// Custom tooltip
// ---------------------------------------------------------------------------

function GanttTooltip(props: { active?: boolean; payload?: Array<{ payload?: GanttRow }> }) {
  const { active, payload } = props;
  if (!active || !payload?.length) return null;
  const row = payload[0]?.payload;
  if (!row) return null;

  return (
    <div
      className="rounded-md px-3 py-2 text-xs shadow-lg"
      style={{
        backgroundColor: "hsl(224, 71%, 6%)",
        border: "1px solid hsl(216, 34%, 17%)",
        color: "hsl(213, 31%, 91%)",
      }}
    >
      <p className="font-semibold mb-1">{row.label}</p>
      <p className="text-muted-foreground">Phase: {row.phaseTitle}</p>
      <p>
        Status: <span style={{ color: STATUS_BAR_COLORS[row.status] }}>{row.status}</span>
      </p>
      <p>Est. duration: {row.complexityHours}h</p>
      <p>Start offset: {row.offset}h</p>
      {row.agentId && <p className="font-mono mt-0.5">Agent: {row.agentId}</p>}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Public component
// ---------------------------------------------------------------------------

interface GanttChartProps {
  plan: OrchestratorPlan;
  onStageClick?: (stageId: string) => void;
}

export function GanttChart({ plan, onStageClick }: GanttChartProps) {
  const rows = useMemo(() => planToGanttRows(plan), [plan]);

  const chartHeight = Math.max(200, rows.length * 36 + 60);

  const handleBarClick = (data: GanttRow) => {
    onStageClick?.(data.stageId);
  };

  return (
    <div className="w-full rounded-lg border border-border overflow-hidden bg-background p-4">
      <ResponsiveContainer width="100%" height={chartHeight}>
        <BarChart
          data={rows}
          layout="vertical"
          margin={{ top: 5, right: 20, bottom: 5, left: 10 }}
          barSize={20}
        >
          <CartesianGrid
            strokeDasharray="3 3"
            stroke="hsl(216, 34%, 17%)"
            horizontal={false}
          />
          <XAxis
            type="number"
            tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
            label={{
              value: "Hours",
              position: "insideBottomRight",
              offset: -5,
              style: { fontSize: 11, fill: "hsl(215, 16%, 57%)" },
            }}
          />
          <YAxis
            type="category"
            dataKey="label"
            width={180}
            tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
          />
          <Tooltip content={<GanttTooltip />} cursor={{ fill: "hsl(220, 14%, 12%)" }} />

          {/* Invisible offset bar to push the visible bar rightward */}
          <Bar dataKey="offset" stackId="gantt" fill="transparent" isAnimationActive={false} />

          {/* Visible duration bar */}
          <Bar
            dataKey="duration"
            stackId="gantt"
            radius={[0, 4, 4, 0]}
            onClick={(_data, _index, e) => {
              const row = (e as unknown as { payload: GanttRow })?.payload;
              if (row) handleBarClick(row);
            }}
            cursor="pointer"
          >
            {rows.map((row) => (
              <Cell
                key={row.stageId}
                fill={STATUS_BAR_COLORS[row.status]}
                stroke={row.status === "running" ? "hsl(210, 70%, 65%)" : undefined}
                strokeWidth={row.status === "running" ? 1.5 : 0}
              />
            ))}
          </Bar>
        </BarChart>
      </ResponsiveContainer>
    </div>
  );
}
