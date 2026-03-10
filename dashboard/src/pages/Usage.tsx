import { useEffect } from "react";
import { BarChart3 } from "lucide-react";
import { useUsageStore } from "@/stores/usage";
import { AgentUsageBarChart, CumulativeUsageChart, SessionTimelineChart } from "@/components/UsageChart";
import { CostBreakdown } from "@/components/CostBreakdown";
import { Card, CardContent } from "@/components/ui/card";

export function Usage() {
  const { summary, budget, loading, error, fetchSummary, fetchBudget, updateBudget } =
    useUsageStore();

  useEffect(() => {
    void fetchSummary();
    void fetchBudget();
  }, [fetchSummary, fetchBudget]);

  if (loading && !summary) {
    return (
      <div className="p-6">
        <p className="text-muted-foreground text-sm">Loading usage data…</p>
      </div>
    );
  }

  if (error && !summary) {
    return (
      <div className="p-6">
        <p className="text-destructive text-sm">Error: {error}</p>
      </div>
    );
  }

  if (!summary || (summary.by_agent.length === 0 && summary.daily.length === 0)) {
    return (
      <div className="p-6 space-y-4">
        <h1 className="text-2xl font-bold">Usage</h1>
        <Card>
          <CardContent className="py-12 text-center">
            <BarChart3 className="h-10 w-10 mx-auto mb-3 text-muted-foreground/40" />
            <p className="text-muted-foreground text-sm">
              No token usage recorded yet. Usage data is collected as agents run.
            </p>
          </CardContent>
        </Card>
      </div>
    );
  }

  const effectiveBudget = budget ?? {
    daily_limit: null,
    monthly_limit: null,
    alert_threshold_percent: 80,
  };

  return (
    <div className="p-6 space-y-6">
      <h1 className="text-2xl font-bold">Usage</h1>

      {/* Cost breakdown + budget alerts */}
      <CostBreakdown
        summary={summary}
        budget={effectiveBudget}
        onUpdateBudget={updateBudget}
      />

      {/* Per-agent token usage bar chart */}
      <AgentUsageBarChart data={summary.by_agent} />

      {/* Daily consumption timeline */}
      <SessionTimelineChart data={summary.daily} />

      {/* Cumulative usage line chart */}
      <CumulativeUsageChart data={summary.daily} />
    </div>
  );
}
