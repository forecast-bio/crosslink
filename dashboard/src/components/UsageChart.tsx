import {
  BarChart,
  Bar,
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer,
} from "recharts";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import type { AgentUsageSummary, DailyUsage } from "@/lib/types";

/** Format large token counts as compact strings (e.g. 1.2M, 450K). */
function formatTokens(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(0)}K`;
  return String(value);
}

const CHART_COLORS = {
  input: "hsl(210, 70%, 55%)",
  output: "hsl(150, 60%, 45%)",
  cost: "hsl(35, 85%, 55%)",
};

const TOOLTIP_STYLE = {
  contentStyle: {
    backgroundColor: "hsl(224, 71%, 6%)",
    border: "1px solid hsl(216, 34%, 17%)",
    borderRadius: "6px",
    color: "hsl(213, 31%, 91%)",
    fontSize: "12px",
  },
  labelStyle: { color: "hsl(213, 31%, 91%)" },
};

interface AgentUsageBarChartProps {
  data: AgentUsageSummary[];
}

/** Per-agent token usage bar chart (input vs output tokens). */
export function AgentUsageBarChart({ data }: AgentUsageBarChartProps) {
  if (data.length === 0) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="text-sm">Token Usage by Agent</CardTitle>
        </CardHeader>
        <CardContent className="py-8 text-center">
          <p className="text-sm text-muted-foreground">No usage data available.</p>
        </CardContent>
      </Card>
    );
  }

  const chartData = data.map((a) => ({
    agent: a.agent_id.length > 20 ? a.agent_id.slice(0, 18) + "…" : a.agent_id,
    full_agent: a.agent_id,
    input: a.input_tokens,
    output: a.output_tokens,
  }));

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm">Token Usage by Agent</CardTitle>
      </CardHeader>
      <CardContent>
        <ResponsiveContainer width="100%" height={300}>
          <BarChart data={chartData} margin={{ top: 5, right: 20, bottom: 60, left: 10 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="hsl(216, 34%, 17%)" />
            <XAxis
              dataKey="agent"
              tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
              angle={-35}
              textAnchor="end"
              height={70}
            />
            <YAxis
              tickFormatter={formatTokens}
              tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
            />
            <Tooltip
              {...TOOLTIP_STYLE}
              formatter={(value: number, name: string) => [
                formatTokens(value),
                name === "input" ? "Input tokens" : "Output tokens",
              ]}
              labelFormatter={(_label, payload) =>
                payload?.[0]?.payload?.full_agent ?? _label
              }
            />
            <Legend
              wrapperStyle={{ fontSize: "12px", color: "hsl(215, 16%, 57%)" }}
            />
            <Bar dataKey="input" name="Input tokens" fill={CHART_COLORS.input} radius={[3, 3, 0, 0]} />
            <Bar dataKey="output" name="Output tokens" fill={CHART_COLORS.output} radius={[3, 3, 0, 0]} />
          </BarChart>
        </ResponsiveContainer>
      </CardContent>
    </Card>
  );
}

interface CumulativeUsageChartProps {
  data: DailyUsage[];
}

/** Cumulative token usage over time (line chart). */
export function CumulativeUsageChart({ data }: CumulativeUsageChartProps) {
  if (data.length === 0) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="text-sm">Cumulative Usage Over Time</CardTitle>
        </CardHeader>
        <CardContent className="py-8 text-center">
          <p className="text-sm text-muted-foreground">No daily usage data available.</p>
        </CardContent>
      </Card>
    );
  }

  // Build cumulative series
  let cumulativeInput = 0;
  let cumulativeOutput = 0;
  let cumulativeCost = 0;
  const cumulativeData = data.map((d) => {
    cumulativeInput += d.input_tokens;
    cumulativeOutput += d.output_tokens;
    cumulativeCost += d.cost_estimate;
    return {
      date: d.date,
      input: cumulativeInput,
      output: cumulativeOutput,
      cost: cumulativeCost,
    };
  });

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm">Cumulative Usage Over Time</CardTitle>
      </CardHeader>
      <CardContent>
        <ResponsiveContainer width="100%" height={300}>
          <LineChart data={cumulativeData} margin={{ top: 5, right: 20, bottom: 5, left: 10 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="hsl(216, 34%, 17%)" />
            <XAxis
              dataKey="date"
              tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
            />
            <YAxis
              yAxisId="tokens"
              tickFormatter={formatTokens}
              tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
            />
            <YAxis
              yAxisId="cost"
              orientation="right"
              tickFormatter={(v: number) => `$${v.toFixed(2)}`}
              tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
            />
            <Tooltip
              {...TOOLTIP_STYLE}
              formatter={(value: number, name: string) => {
                if (name === "Cost") return [`$${value.toFixed(4)}`, name];
                return [formatTokens(value), name];
              }}
            />
            <Legend
              wrapperStyle={{ fontSize: "12px", color: "hsl(215, 16%, 57%)" }}
            />
            <Line
              yAxisId="tokens"
              type="monotone"
              dataKey="input"
              name="Input tokens"
              stroke={CHART_COLORS.input}
              strokeWidth={2}
              dot={false}
            />
            <Line
              yAxisId="tokens"
              type="monotone"
              dataKey="output"
              name="Output tokens"
              stroke={CHART_COLORS.output}
              strokeWidth={2}
              dot={false}
            />
            <Line
              yAxisId="cost"
              type="monotone"
              dataKey="cost"
              name="Cost"
              stroke={CHART_COLORS.cost}
              strokeWidth={2}
              strokeDasharray="5 5"
              dot={false}
            />
          </LineChart>
        </ResponsiveContainer>
      </CardContent>
    </Card>
  );
}

interface SessionTimelineChartProps {
  data: DailyUsage[];
}

/** Daily input/output token consumption as stacked bars. */
export function SessionTimelineChart({ data }: SessionTimelineChartProps) {
  if (data.length === 0) return null;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm">Daily Token Consumption</CardTitle>
      </CardHeader>
      <CardContent>
        <ResponsiveContainer width="100%" height={250}>
          <BarChart data={data} margin={{ top: 5, right: 20, bottom: 5, left: 10 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="hsl(216, 34%, 17%)" />
            <XAxis
              dataKey="date"
              tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
            />
            <YAxis
              tickFormatter={formatTokens}
              tick={{ fontSize: 11, fill: "hsl(215, 16%, 57%)" }}
            />
            <Tooltip
              {...TOOLTIP_STYLE}
              formatter={(value: number, name: string) => [
                formatTokens(value),
                name,
              ]}
            />
            <Legend
              wrapperStyle={{ fontSize: "12px", color: "hsl(215, 16%, 57%)" }}
            />
            <Bar
              dataKey="input_tokens"
              name="Input tokens"
              stackId="tokens"
              fill={CHART_COLORS.input}
              radius={[0, 0, 0, 0]}
            />
            <Bar
              dataKey="output_tokens"
              name="Output tokens"
              stackId="tokens"
              fill={CHART_COLORS.output}
              radius={[3, 3, 0, 0]}
            />
          </BarChart>
        </ResponsiveContainer>
      </CardContent>
    </Card>
  );
}
