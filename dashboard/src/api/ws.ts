// WebSocket subscription for server-pushed dashboard events.
//
// The server publishes `dashboard_project_updated` events on its
// single `/ws` endpoint whenever the poll loop finishes writing a
// project's updated state. We subscribe to the `"dashboard"` channel
// only (filter out noise from the legacy single-project channels)
// and invalidate the matching React Query cache so the frontend
// refetches ahead of the 5-second polling fallback.
//
// Reconnect logic is intentionally simple — on disconnect we wait a
// short backoff and try again. No exponential backoff for MVP; the
// panel is local-network so disconnects are rare.

import type { QueryClient } from "@tanstack/react-query";

const WS_URL = () => {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  // Token query-string param is auto-installed by auth/bootstrap.ts's
  // fetch wrapper for REST calls, but WebSocket doesn't go through
  // fetch. Pull the stored token and attach it to the URL.
  const token = window.sessionStorage.getItem("crosslink_api_token");
  const query = token ? `?token=${encodeURIComponent(token)}` : "";
  return `${proto}//${window.location.host}/ws${query}`;
};

interface DashboardProjectUpdated {
  type: "dashboard_project_updated";
  slug: string;
  seq: number;
}

interface DashboardAlertsChanged {
  type: "dashboard_alerts_changed";
  slug: string;
  opened: number;
  resolved: number;
  seq: number;
}

type IncomingEnvelope =
  | DashboardProjectUpdated
  | DashboardAlertsChanged
  | { type: string; seq: number };

/// Connect to `/ws`, subscribe to the `"dashboard"` channel, and wire
/// incoming events to React Query cache invalidations. Returns a
/// disposer that closes the socket.
export function connectDashboardWs(queryClient: QueryClient): () => void {
  let closed = false;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let socket: WebSocket | null = null;

  const open = () => {
    if (closed) return;
    socket = new WebSocket(WS_URL());
    socket.onopen = () => {
      // Server accepts an optional `subscribe` filter message after
      // connect. Restrict to just the dashboard channel so we don't
      // process irrelevant single-project events.
      socket?.send(JSON.stringify({ type: "subscribe", channels: ["dashboard"] }));
    };
    socket.onmessage = (ev) => {
      if (typeof ev.data !== "string") return;
      let msg: IncomingEnvelope;
      try {
        msg = JSON.parse(ev.data);
      } catch {
        return;
      }
      if (msg.type === "dashboard_project_updated") {
        // Invalidate the list (updates the tile) and the specific
        // project's detail query (if the user is drilled in).
        const slug = (msg as DashboardProjectUpdated).slug;
        queryClient.invalidateQueries({ queryKey: ["dashboard", "projects"] });
        queryClient.invalidateQueries({ queryKey: ["dashboard", "project", slug] });
      } else if (msg.type === "dashboard_alerts_changed") {
        // Alert set changed for some project — invalidate the global
        // alerts query so the rail + /alerts page catch up immediately.
        queryClient.invalidateQueries({ queryKey: ["dashboard", "alerts"] });
      }
    };
    socket.onclose = () => {
      if (closed) return;
      // Reconnect after a short delay. The server keeps its broadcast
      // state across reconnects, so we'll resume receiving updates.
      retryTimer = setTimeout(open, 1_000);
    };
    socket.onerror = () => {
      socket?.close();
    };
  };

  open();

  return () => {
    closed = true;
    if (retryTimer) clearTimeout(retryTimer);
    socket?.close();
    socket = null;
  };
}
