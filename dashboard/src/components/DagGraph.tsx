import { useCallback, useMemo } from "react";
import {
  ReactFlow,
  Background,
  Controls,
  type Node,
  type Edge,
  type NodeMouseHandler,
  Position,
  MarkerType,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type { OrchestratorPlan, StageStatus } from "@/lib/types";
import { Badge } from "@/components/ui/badge";

// ---------------------------------------------------------------------------
// Status → visual mapping
// ---------------------------------------------------------------------------

const STATUS_COLORS: Record<StageStatus | "pending", { bg: string; border: string; text: string }> = {
  pending:  { bg: "hsl(220, 14%, 18%)", border: "hsl(220, 13%, 30%)", text: "hsl(215, 16%, 57%)" },
  running:  { bg: "hsl(210, 70%, 12%)", border: "hsl(210, 70%, 45%)", text: "hsl(210, 70%, 65%)" },
  done:     { bg: "hsl(150, 50%, 12%)", border: "hsl(150, 60%, 38%)", text: "hsl(150, 60%, 55%)" },
  failed:   { bg: "hsl(0, 60%, 14%)",   border: "hsl(0, 70%, 45%)",  text: "hsl(0, 70%, 65%)" },
  blocked:  { bg: "hsl(45, 60%, 12%)",  border: "hsl(45, 70%, 45%)", text: "hsl(45, 70%, 60%)" },
  skipped:  { bg: "hsl(220, 14%, 15%)", border: "hsl(220, 13%, 25%)", text: "hsl(215, 16%, 45%)" },
};

function statusBadgeVariant(status: StageStatus | undefined): "success" | "info" | "warning" | "destructive" | "secondary" {
  switch (status) {
    case "done":    return "success";
    case "running": return "info";
    case "blocked": return "warning";
    case "failed":  return "destructive";
    default:        return "secondary";
  }
}

// ---------------------------------------------------------------------------
// Custom node component
// ---------------------------------------------------------------------------

interface StageNodeData {
  label: string;
  status: StageStatus;
  agentId?: string;
  phaseTitle: string;
  complexityHours: number;
  [key: string]: unknown;
}

function StageNode({ data }: { data: StageNodeData }) {
  const colors = STATUS_COLORS[data.status] ?? STATUS_COLORS.pending;
  const isRunning = data.status === "running";

  return (
    <div
      className="rounded-lg px-3 py-2 min-w-[160px] max-w-[240px] shadow-md"
      style={{
        backgroundColor: colors.bg,
        border: `2px solid ${colors.border}`,
        color: colors.text,
      }}
    >
      {isRunning && (
        <div
          className="absolute inset-0 rounded-lg animate-pulse pointer-events-none"
          style={{
            boxShadow: `0 0 12px ${colors.border}`,
            opacity: 0.4,
          }}
        />
      )}
      <div className="flex items-center justify-between gap-2 mb-1">
        <span className="text-[11px] font-medium truncate" style={{ color: "hsl(215, 16%, 57%)" }}>
          {data.phaseTitle}
        </span>
        <Badge variant={statusBadgeVariant(data.status)} className="text-[10px] px-1.5 py-0">
          {data.status}
        </Badge>
      </div>
      <p className="text-sm font-semibold leading-tight truncate" style={{ color: "hsl(213, 31%, 91%)" }}>
        {data.label}
      </p>
      <div className="flex items-center justify-between mt-1.5">
        {data.agentId ? (
          <span className="text-[10px] font-mono truncate max-w-[120px]">{data.agentId}</span>
        ) : (
          <span className="text-[10px] italic">unassigned</span>
        )}
        <span className="text-[10px]">{data.complexityHours}h</span>
      </div>
    </div>
  );
}

const nodeTypes = { stage: StageNode };

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

const NODE_WIDTH = 200;
const NODE_HEIGHT = 90;
const HORIZONTAL_GAP = 60;
const VERTICAL_GAP = 40;

interface LayoutResult {
  nodes: Node<StageNodeData>[];
  edges: Edge[];
}

/**
 * Simple layered layout: assign each stage a depth (longest path from a root)
 * and stack stages at the same depth vertically.
 */
function layoutPlan(plan: OrchestratorPlan): LayoutResult {
  const allStages = plan.phases.flatMap((p) =>
    p.stages.map((s) => ({ ...s, phaseTitle: p.title })),
  );

  // Build adjacency for depth calculation
  const stageById = new Map(allStages.map((s) => [s.id, s]));
  const depths = new Map<string, number>();

  function getDepth(id: string, visited: Set<string>): number {
    if (depths.has(id)) return depths.get(id)!;
    if (visited.has(id)) return 0; // cycle guard
    visited.add(id);
    const stage = stageById.get(id);
    if (!stage || stage.depends_on.length === 0) {
      depths.set(id, 0);
      return 0;
    }
    const maxParent = Math.max(
      ...stage.depends_on.map((dep) => (stageById.has(dep) ? getDepth(dep, visited) + 1 : 0)),
    );
    depths.set(id, maxParent);
    return maxParent;
  }

  for (const s of allStages) getDepth(s.id, new Set());

  // Group by depth
  const byDepth = new Map<number, typeof allStages>();
  for (const s of allStages) {
    const d = depths.get(s.id) ?? 0;
    if (!byDepth.has(d)) byDepth.set(d, []);
    byDepth.get(d)!.push(s);
  }

  const maxDepth = Math.max(...byDepth.keys(), 0);

  const nodes: Node<StageNodeData>[] = [];
  const edges: Edge[] = [];

  for (let col = 0; col <= maxDepth; col++) {
    const group = byDepth.get(col) ?? [];
    const totalHeight = group.length * NODE_HEIGHT + (group.length - 1) * VERTICAL_GAP;
    const startY = -totalHeight / 2;

    group.forEach((stage, row) => {
      nodes.push({
        id: stage.id,
        type: "stage",
        position: {
          x: col * (NODE_WIDTH + HORIZONTAL_GAP),
          y: startY + row * (NODE_HEIGHT + VERTICAL_GAP),
        },
        data: {
          label: stage.title,
          status: stage.status ?? "pending",
          agentId: stage.agent_id,
          phaseTitle: stage.phaseTitle,
          complexityHours: stage.complexity_hours,
        },
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
      });

      for (const dep of stage.depends_on) {
        if (stageById.has(dep)) {
          edges.push({
            id: `${dep}->${stage.id}`,
            source: dep,
            target: stage.id,
            animated: stage.status === "running",
            style: {
              stroke: stage.status === "running"
                ? "hsl(210, 70%, 55%)"
                : "hsl(220, 13%, 30%)",
              strokeWidth: 2,
            },
            markerEnd: {
              type: MarkerType.ArrowClosed,
              color: stage.status === "running"
                ? "hsl(210, 70%, 55%)"
                : "hsl(220, 13%, 30%)",
              width: 16,
              height: 16,
            },
          });
        }
      }
    });
  }

  return { nodes, edges };
}

// ---------------------------------------------------------------------------
// Public component
// ---------------------------------------------------------------------------

interface DagGraphProps {
  plan: OrchestratorPlan;
  onStageClick?: (stageId: string) => void;
}

export function DagGraph({ plan, onStageClick }: DagGraphProps) {
  const { nodes, edges } = useMemo(() => layoutPlan(plan), [plan]);

  const handleNodeClick: NodeMouseHandler<Node<StageNodeData>> = useCallback(
    (_event, node) => {
      onStageClick?.(node.id);
    },
    [onStageClick],
  );

  return (
    <div className="w-full h-[500px] rounded-lg border border-border overflow-hidden bg-background">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={nodeTypes}
        onNodeClick={handleNodeClick}
        fitView
        fitViewOptions={{ padding: 0.2 }}
        proOptions={{ hideAttribution: true }}
        minZoom={0.3}
        maxZoom={2}
        nodesDraggable={false}
        nodesConnectable={false}
        defaultEdgeOptions={{ type: "smoothstep" }}
      >
        <Background color="hsl(220, 13%, 20%)" gap={20} size={1} />
        <Controls
          showInteractive={false}
          className="[&_button]:bg-muted [&_button]:border-border [&_button]:text-foreground [&_button:hover]:bg-accent"
        />
      </ReactFlow>
    </div>
  );
}
