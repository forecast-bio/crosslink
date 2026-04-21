// App shell for the multi-project dashboard (GH #429).
// Wires the QueryClient + BrowserRouter and hands off to the
// top-level routes. Real SCADA layout (sidebar, global alert rail,
// terminal list) lands in later P1.* subissues.

import { useEffect } from "react";
import { QueryClient, QueryClientProvider, useQueryClient } from "@tanstack/react-query";
import { BrowserRouter, Route, Routes } from "react-router-dom";

import { Link, NavLink } from "react-router-dom";

import { connectDashboardWs } from "@/api/ws";
import { AlertRail } from "@/components/AlertRail";
import { Alerts } from "@/pages/Alerts";
import { ProjectDetail } from "@/pages/ProjectDetail";
import { ProjectGrid } from "@/pages/ProjectGrid";
import { SettingsGithub } from "@/pages/SettingsGithub";
import { Terminals } from "@/pages/Terminals";

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

function TopNav() {
  const linkClass = ({ isActive }: { isActive: boolean }) =>
    `rounded px-2 py-1 text-xs uppercase tracking-wide hover:bg-accent/10 ${
      isActive ? "bg-accent/20 font-semibold" : "text-muted-foreground"
    }`;
  return (
    <nav className="border-b border-border bg-card/60">
      <div className="mx-auto flex max-w-6xl items-center gap-3 px-6 py-2 text-sm">
        <Link to="/" className="font-semibold tracking-tight">
          crosslink dashboard
        </Link>
        <span className="ml-4 flex items-center gap-1">
          <NavLink to="/" end className={linkClass}>
            Projects
          </NavLink>
          <NavLink to="/alerts" className={linkClass}>
            Alerts
          </NavLink>
          <NavLink to="/terminals" className={linkClass}>
            Terminals
          </NavLink>
          <NavLink to="/settings/github" className={linkClass}>
            GitHub
          </NavLink>
        </span>
      </div>
    </nav>
  );
}

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <DashboardWsBridge />
      <BrowserRouter>
        <div className="min-h-screen bg-background text-foreground">
          <TopNav />
          <AlertRail />
          <Routes>
            <Route path="/" element={<ProjectGrid />} />
            <Route path="/project/*" element={<ProjectDetail />} />
            <Route path="/alerts" element={<Alerts />} />
            <Route path="/terminals" element={<Terminals />} />
            <Route path="/settings/github" element={<SettingsGithub />} />
          </Routes>
        </div>
      </BrowserRouter>
    </QueryClientProvider>
  );
}
