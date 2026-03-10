import { useEffect, useState } from "react";
import { milestones as milestonesApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import type { MilestoneDetail } from "@/lib/types";

export function Milestones() {
  const [items, setItems] = useState<MilestoneDetail[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    milestonesApi
      .list()
      .then(setItems)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  return (
    <div className="p-6 space-y-4">
      <h1 className="text-2xl font-bold">Milestones</h1>
      {loading ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : items.length === 0 ? (
        <p className="text-muted-foreground text-sm">No milestones found.</p>
      ) : (
        <div className="space-y-3">
          {items.map((m) => {
            const pct =
              m.issue_count > 0
                ? Math.round((m.completed_count / m.issue_count) * 100)
                : 0;
            return (
              <Card key={m.id}>
                <CardHeader className="pb-2">
                  <CardTitle className="text-base flex items-center justify-between">
                    {m.name}
                    <Badge variant={m.status === "open" ? "success" : "secondary"}>
                      {m.status}
                    </Badge>
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
                    <span>{pct}%</span>
                  </div>
                  <div className="h-1.5 w-full rounded-full bg-secondary overflow-hidden">
                    <div
                      className="h-full rounded-full bg-blue-500 transition-all"
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                </CardContent>
              </Card>
            );
          })}
        </div>
      )}
    </div>
  );
}
