// App shell for the multi-project dashboard (GH #429).
// Wires the QueryClient + BrowserRouter and hands off to the
// top-level routes. Real SCADA layout (sidebar, global alert rail,
// terminal list) lands in later P1.* subissues.

import { useEffect } from "react";
import { QueryClient, QueryClientProvider, useQueryClient } from "@tanstack/react-query";
import { BrowserRouter, Route, Routes } from "react-router-dom";

import { connectDashboardWs } from "@/api/ws";
import { AlertRail } from "@/components/AlertRail";
import { Alerts } from "@/pages/Alerts";
import { ProjectDetail } from "@/pages/ProjectDetail";
import { ProjectGrid } from "@/pages/ProjectGrid";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Errors bubble to components; no auto-retry on 4xx noise.
      retry: false,
      refetchOnWindowFocus: false,
    },
  },
});

/// Keeps a WebSocket subscription alive for the app's lifetime.
/// Any server-emitted dashboard event invalidates the relevant
/// React Query cache entries so tiles refresh without waiting for
/// the 5-second polling fallback.
function DashboardWsBridge() {
  const client = useQueryClient();
  useEffect(() => {
    return connectDashboardWs(client);
  }, [client]);
  return null;
}

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <DashboardWsBridge />
      <BrowserRouter>
        <div className="min-h-screen bg-background text-foreground">
          <AlertRail />
          <Routes>
            <Route path="/" element={<ProjectGrid />} />
            <Route path="/project/*" element={<ProjectDetail />} />
            <Route path="/alerts" element={<Alerts />} />
          </Routes>
        </div>
      </BrowserRouter>
    </QueryClientProvider>
  );
}
