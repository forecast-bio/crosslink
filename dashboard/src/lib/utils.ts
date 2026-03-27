import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";
import type { IssuePriority } from "@/lib/types";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function formatRelativeTime(isoString: string): string {
  const date = new Date(isoString);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffSec = Math.floor(diffMs / 1000);

  if (diffSec < 60) return `${diffSec}s ago`;
  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) return `${diffMin}m ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h ago`;
  const diffDay = Math.floor(diffHr / 24);
  return `${diffDay}d ago`;
}

export function formatDateTime(isoString: string): string {
  return new Date(isoString).toLocaleString();
}

export function isStale(isoString: string, thresholdMs = 120_000): boolean {
  return Date.now() - new Date(isoString).getTime() > thresholdMs;
}

export function priorityVariant(p: IssuePriority): "destructive" | "warning" | "info" | "secondary" {
  switch (p) {
    case "critical": return "destructive";
    case "high": return "warning";
    case "medium": return "info";
    default: return "secondary";
  }
}
