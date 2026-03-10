import { useMemo } from "react";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";

interface HeartbeatSegment {
  active: boolean;
  count: number;
  start: Date;
  end: Date;
}

interface Props {
  /** ISO 8601 timestamps from the last N hours, oldest first. */
  timestamps: string[];
  /** How many hours to show. Defaults to 24. */
  windowHours?: number;
}

/**
 * Renders a horizontal timeline bar showing active/idle periods over the last
 * `windowHours` hours, derived from an array of heartbeat timestamps.
 *
 * The bar is divided into 30-minute segments. A segment is "active" if at
 * least one heartbeat timestamp falls within it.
 */
export function HeartbeatTimeline({ timestamps, windowHours = 24 }: Props) {
  const SEGMENTS = windowHours * 2; // one segment per 30 minutes

  const segments = useMemo<HeartbeatSegment[]>(() => {
    const now = Date.now();
    const windowMs = windowHours * 60 * 60 * 1000;
    const segmentMs = windowMs / SEGMENTS;

    // Pre-parse all timestamps for performance
    const times = timestamps.map((ts) => new Date(ts).getTime());

    return Array.from({ length: SEGMENTS }, (_, i) => {
      const segStart = now - windowMs + i * segmentMs;
      const segEnd = segStart + segmentMs;
      const count = times.filter((t) => t >= segStart && t < segEnd).length;
      return {
        active: count > 0,
        count,
        start: new Date(segStart),
        end: new Date(segEnd),
      };
    });
  }, [timestamps, windowHours, SEGMENTS]);

  const activeCount = segments.filter((s) => s.active).length;
  const uptimePct = SEGMENTS > 0 ? Math.round((activeCount / SEGMENTS) * 100) : 0;
  const totalHeartbeats = timestamps.length;

  if (timestamps.length === 0) {
    return (
      <div className="space-y-2">
        <div className="h-8 rounded bg-muted flex items-center justify-center">
          <span className="text-xs text-muted-foreground">No heartbeat data</span>
        </div>
        <div className="flex justify-between text-xs text-muted-foreground">
          <span>{windowHours}h ago</span>
          <span>now</span>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {/* Legend + summary */}
      <div className="flex items-center gap-3 text-xs text-muted-foreground flex-wrap">
        <div className="flex items-center gap-1.5">
          <div className="w-2.5 h-2.5 rounded-sm bg-green-500" />
          <span>active</span>
        </div>
        <div className="flex items-center gap-1.5">
          <div className="w-2.5 h-2.5 rounded-sm bg-muted border border-border" />
          <span>idle</span>
        </div>
        <span className="ml-auto">
          {uptimePct}% uptime · {totalHeartbeats} heartbeats
        </span>
      </div>

      {/* Timeline bar */}
      <TooltipProvider>
        <div className="flex gap-px h-8 rounded overflow-hidden">
          {segments.map((seg, i) => (
            <Tooltip key={i}>
              <TooltipTrigger asChild>
                <div
                  className={`flex-1 cursor-default transition-opacity hover:opacity-70 ${
                    seg.active ? "bg-green-500" : "bg-muted"
                  }`}
                />
              </TooltipTrigger>
              <TooltipContent side="top" className="text-xs">
                <p className="font-medium">
                  {seg.start.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                  {" – "}
                  {seg.end.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                </p>
                <p className="text-muted-foreground">
                  {seg.active
                    ? `${seg.count} heartbeat${seg.count !== 1 ? "s" : ""}`
                    : "no heartbeats"}
                </p>
              </TooltipContent>
            </Tooltip>
          ))}
        </div>
      </TooltipProvider>

      {/* Time axis labels */}
      <div className="flex justify-between text-xs text-muted-foreground">
        <span>{windowHours}h ago</span>
        {windowHours >= 12 && (
          <span className="hidden sm:block">{windowHours / 2}h ago</span>
        )}
        <span>now</span>
      </div>
    </div>
  );
}
