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
import { installAlertSoundBridge } from "@/lib/alertSound";
import {
  patchPreferences,
  usePreferences,
  type ThemePreference,
} from "@/lib/preferences";
import { resolveTheme, useThemeBridge } from "@/lib/theme";
import { Alerts } from "@/pages/Alerts";
import { ProjectDetail } from "@/pages/ProjectDetail";
import { ProjectGrid } from "@/pages/ProjectGrid";
import { SettingsGithub } from "@/pages/SettingsGithub";
import { SettingsPreferences } from "@/pages/SettingsPreferences";
import { SettingsWebhooks } from "@/pages/SettingsWebhooks";
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

/// Mounts the alert-sound bridge for the app lifetime. Split from
/// DashboardWsBridge so it can be disabled in isolation during tests.
function AlertSoundBridge() {
  useEffect(() => installAlertSoundBridge(), []);
  return null;
}

/// One-click light/dark toggle in the nav. Leaves "system" mode alone
/// unless the user wanted it; first click from "system" picks the
/// opposite of what's currently rendered so the visible change is
/// immediate.
function ThemeToggleButton() {
  const prefs = usePreferences();
  const resolved = resolveTheme(prefs.theme);
  const next: ThemePreference = resolved === "dark" ? "light" : "dark";
  const label = resolved === "dark" ? "Switch to light mode" : "Switch to dark mode";
  return (
    <button
      type="button"
      onClick={() => patchPreferences({ theme: next })}
      aria-label={label}
      title={label}
      className="rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent/10"
    >
      {resolved === "dark" ? "☀︎" : "☾"}
    </button>
  );
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
          <NavLink to="/settings/webhooks" className={linkClass}>
            Webhooks
          </NavLink>
          <NavLink to="/settings/preferences" className={linkClass}>
            Preferences
          </NavLink>
        </span>
        <span className="ml-auto">
          <ThemeToggleButton />
        </span>
      </div>
    </nav>
  );
}

function AppShell() {
  useThemeBridge();
  return (
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
          <Route path="/settings/webhooks" element={<SettingsWebhooks />} />
          <Route
            path="/settings/preferences"
            element={<SettingsPreferences />}
          />
        </Routes>
      </div>
    </BrowserRouter>
  );
}

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <DashboardWsBridge />
      <AlertSoundBridge />
      <AppShell />
    </QueryClientProvider>
  );
}
