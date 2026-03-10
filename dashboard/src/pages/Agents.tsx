import { useEffect } from "react";
import { Bot } from "lucide-react";
import { useAgentsStore } from "@/stores/agents";
import { AgentCard } from "@/components/AgentCard";
import { Card, CardContent } from "@/components/ui/card";

export function Agents() {
  const { agents, loading, fetch } = useAgentsStore();

  useEffect(() => { void fetch(); }, [fetch]);

  return (
    <div className="p-6 space-y-4">
      <h1 className="text-2xl font-bold">Agents</h1>

      {loading && agents.length === 0 ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : agents.length === 0 ? (
        <Card>
          <CardContent className="py-12 text-center">
            <Bot className="h-10 w-10 mx-auto mb-3 text-muted-foreground/40" />
            <p className="text-muted-foreground text-sm">
              No active agents. Launch one with{" "}
              <code className="text-blue-400">crosslink kickoff run</code>
            </p>
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <AgentCard key={agent.agent_id} agent={agent} />
          ))}
        </div>
      )}
    </div>
  );
}
