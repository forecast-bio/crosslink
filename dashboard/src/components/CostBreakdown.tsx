import { useState } from "react";
import { AlertTriangle, DollarSign, Cpu, TrendingUp } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import type { BudgetConfig, ModelUsageSummary, UsageSummary } from "@/lib/types";

/** Format a dollar amount to a readable string. */
function formatCost(value: number): string {
  if (value >= 100) return `$${value.toFixed(2)}`;
  if (value >= 1) return `$${value.toFixed(3)}`;
  return `$${value.toFixed(4)}`;
}

function formatTokens(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(0)}K`;
  return String(value);
}

// ── Budget Alert ─────────────────────────────────────────────────────────────

interface BudgetAlertProps {
  label: string;
  spent: number;
  limit: number;
  thresholdPercent: number;
}

function BudgetAlert({ label, spent, limit, thresholdPercent }: BudgetAlertProps) {
  const pct = limit > 0 ? (spent / limit) * 100 : 0;
  const isWarning = pct >= thresholdPercent;
  const isOver = pct >= 100;

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between text-sm">
        <span className="text-muted-foreground">{label}</span>
        <span className={cn(isOver ? "text-red-400 font-medium" : isWarning ? "text-yellow-400" : "text-foreground")}>
          {formatCost(spent)} / {formatCost(limit)}
        </span>
      </div>
      <div className="h-2 w-full rounded-full bg-secondary overflow-hidden">
        <div
          className={cn(
            "h-full rounded-full transition-all duration-500",
            isOver ? "bg-red-500" : isWarning ? "bg-yellow-500" : "bg-blue-500",
          )}
          style={{ width: `${Math.min(pct, 100)}%` }}
        />
      </div>
      {isWarning && (
        <div className="flex items-center gap-1.5 text-xs">
          <AlertTriangle className={cn("h-3 w-3", isOver ? "text-red-400" : "text-yellow-400")} />
          <span className={cn(isOver ? "text-red-400" : "text-yellow-400")}>
            {isOver
              ? `Over budget by ${formatCost(spent - limit)}`
              : `${pct.toFixed(0)}% of budget used`}
          </span>
        </div>
      )}
    </div>
  );
}

// ── Model Cost Table ─────────────────────────────────────────────────────────

interface ModelCostTableProps {
  models: ModelUsageSummary[];
}

function ModelCostTable({ models }: ModelCostTableProps) {
  if (models.length === 0) return null;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm">Cost by Model</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="space-y-3">
          {models.map((m) => {
            const total = m.input_tokens + m.output_tokens;
            return (
              <div key={m.model} className="flex items-center justify-between">
                <div className="flex items-center gap-2 min-w-0">
                  <Cpu className="h-4 w-4 text-muted-foreground shrink-0" />
                  <div className="min-w-0">
                    <p className="text-sm font-medium truncate">{m.model}</p>
                    <p className="text-xs text-muted-foreground">
                      {formatTokens(total)} tokens ({formatTokens(m.input_tokens)} in / {formatTokens(m.output_tokens)} out)
                    </p>
                  </div>
                </div>
                <Badge variant="outline" className="shrink-0 ml-2">
                  {formatCost(m.cost_estimate)}
                </Badge>
              </div>
            );
          })}
        </div>
      </CardContent>
    </Card>
  );
}

// ── Budget Config Dialog ─────────────────────────────────────────────────────

interface BudgetDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  budget: BudgetConfig;
  onSave: (budget: Partial<BudgetConfig>) => void;
}

function BudgetDialog({ open, onOpenChange, budget, onSave }: BudgetDialogProps) {
  const [daily, setDaily] = useState(budget.daily_limit?.toString() ?? "");
  const [monthly, setMonthly] = useState(budget.monthly_limit?.toString() ?? "");
  const [threshold, setThreshold] = useState(String(budget.alert_threshold_percent));

  function handleSave() {
    onSave({
      daily_limit: daily ? Number(daily) : null,
      monthly_limit: monthly ? Number(monthly) : null,
      alert_threshold_percent: Number(threshold) || 80,
    });
    onOpenChange(false);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Budget Thresholds</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-1.5">
            <label className="text-sm text-muted-foreground">Daily limit ($)</label>
            <Input
              type="number"
              step="0.01"
              min="0"
              placeholder="No limit"
              value={daily}
              onChange={(e) => setDaily(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm text-muted-foreground">Monthly limit ($)</label>
            <Input
              type="number"
              step="0.01"
              min="0"
              placeholder="No limit"
              value={monthly}
              onChange={(e) => setMonthly(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm text-muted-foreground">Alert threshold (%)</label>
            <Input
              type="number"
              min="1"
              max="100"
              value={threshold}
              onChange={(e) => setThreshold(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              Show a warning when usage reaches this percentage of the limit.
            </p>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button onClick={handleSave}>Save</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ── Main CostBreakdown ───────────────────────────────────────────────────────

interface CostBreakdownProps {
  summary: UsageSummary;
  budget: BudgetConfig;
  onUpdateBudget: (data: Partial<BudgetConfig>) => void;
}

export function CostBreakdown({ summary, budget, onUpdateBudget }: CostBreakdownProps) {
  const [budgetOpen, setBudgetOpen] = useState(false);

  // Compute today's cost from daily data
  const today = new Date().toISOString().slice(0, 10);
  const todayCost = summary.daily.find((d) => d.date === today)?.cost_estimate ?? 0;

  // Compute this month's cost from daily data
  const monthPrefix = today.slice(0, 7);
  const monthlyCost = summary.daily
    .filter((d) => d.date.startsWith(monthPrefix))
    .reduce((sum, d) => sum + d.cost_estimate, 0);

  return (
    <div className="space-y-4">
      {/* Summary stats */}
      <div className="grid gap-3 sm:grid-cols-3">
        <Card>
          <CardContent className="pt-4 pb-3 px-4">
            <div className="flex items-center gap-2 text-muted-foreground text-xs mb-1">
              <DollarSign className="h-3.5 w-3.5" />
              Total Cost
            </div>
            <p className="text-xl font-bold">{formatCost(summary.total_cost)}</p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="pt-4 pb-3 px-4">
            <div className="flex items-center gap-2 text-muted-foreground text-xs mb-1">
              <TrendingUp className="h-3.5 w-3.5" />
              Today
            </div>
            <p className="text-xl font-bold">{formatCost(todayCost)}</p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="pt-4 pb-3 px-4">
            <div className="flex items-center gap-2 text-muted-foreground text-xs mb-1">
              <TrendingUp className="h-3.5 w-3.5" />
              This Month
            </div>
            <p className="text-xl font-bold">{formatCost(monthlyCost)}</p>
          </CardContent>
        </Card>
      </div>

      {/* Budget alerts */}
      {(budget.daily_limit != null || budget.monthly_limit != null) && (
        <Card>
          <CardHeader className="flex flex-row items-center justify-between pb-2">
            <CardTitle className="text-sm">Budget</CardTitle>
            <Button variant="ghost" size="sm" onClick={() => setBudgetOpen(true)}>
              Configure
            </Button>
          </CardHeader>
          <CardContent className="space-y-4">
            {budget.daily_limit != null && (
              <BudgetAlert
                label="Daily"
                spent={todayCost}
                limit={budget.daily_limit}
                thresholdPercent={budget.alert_threshold_percent}
              />
            )}
            {budget.monthly_limit != null && (
              <BudgetAlert
                label="Monthly"
                spent={monthlyCost}
                limit={budget.monthly_limit}
                thresholdPercent={budget.alert_threshold_percent}
              />
            )}
          </CardContent>
        </Card>
      )}

      {/* No budget set — show configure button */}
      {budget.daily_limit == null && budget.monthly_limit == null && (
        <Card>
          <CardContent className="py-6 text-center space-y-2">
            <p className="text-sm text-muted-foreground">No budget limits configured.</p>
            <Button variant="outline" size="sm" onClick={() => setBudgetOpen(true)}>
              Set Budget Thresholds
            </Button>
          </CardContent>
        </Card>
      )}

      {/* Model cost breakdown */}
      <ModelCostTable models={summary.by_model} />

      {/* Budget config dialog */}
      <BudgetDialog
        open={budgetOpen}
        onOpenChange={setBudgetOpen}
        budget={budget}
        onSave={onUpdateBudget}
      />
    </div>
  );
}
