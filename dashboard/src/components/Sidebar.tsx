import { NavLink } from "react-router";
import {
  LayoutDashboard,
  Bot,
  CircleDot,
  BookOpen,
  GitFork,
  Milestone,
  Settings,
  Layers,
  Activity,
  RefreshCw,
  BarChart3,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { SessionPanel } from "@/components/SessionPanel";

interface NavItem {
  label: string;
  to: string;
  icon: React.ComponentType<{ className?: string }>;
}

const NAV_ITEMS: NavItem[] = [
  { label: "Dashboard", to: "/", icon: LayoutDashboard },
  { label: "Agents", to: "/agents", icon: Bot },
  { label: "Issues", to: "/issues", icon: CircleDot },
  { label: "Sessions", to: "/sessions", icon: Activity },
  { label: "Milestones", to: "/milestones", icon: Milestone },
  { label: "Knowledge", to: "/knowledge", icon: BookOpen },
  { label: "Usage", to: "/usage", icon: BarChart3 },
];

const SECONDARY_NAV: NavItem[] = [
  { label: "Sync", to: "/sync", icon: RefreshCw },
  { label: "Orchestrator", to: "/orchestrator", icon: Layers },
  { label: "Execution", to: "/execution", icon: GitFork },
  { label: "Config", to: "/config", icon: Settings },
];

function NavItem({ item }: { item: NavItem }) {
  return (
    <NavLink
      to={item.to}
      end={item.to === "/"}
      className={({ isActive }) =>
        cn(
          "flex items-center gap-3 rounded-md px-3 py-2 text-sm font-medium transition-colors",
          isActive
            ? "bg-sidebar-accent text-sidebar-accent-foreground"
            : "text-muted-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
        )
      }
    >
      <item.icon className="h-4 w-4 shrink-0" />
      {item.label}
    </NavLink>
  );
}

export function Sidebar() {
  return (
    <aside className="flex h-screen w-56 shrink-0 flex-col border-r bg-sidebar-background">
      {/* Logo */}
      <div className="flex h-14 items-center gap-2 border-b border-sidebar-border px-4">
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-blue-500/20">
          <span className="text-xs font-bold text-blue-400">CL</span>
        </div>
        <span className="font-semibold text-foreground">Crosslink</span>
      </div>

      <ScrollArea className="flex-1 px-2 py-3">
        <nav className="space-y-0.5">
          {NAV_ITEMS.map((item) => (
            <NavItem key={item.to} item={item} />
          ))}
        </nav>

        <Separator className="my-3" />

        <nav className="space-y-0.5">
          {SECONDARY_NAV.map((item) => (
            <NavItem key={item.to} item={item} />
          ))}
        </nav>
      </ScrollArea>

      {/* Session status widget */}
      <div className="border-t border-sidebar-border">
        <SessionPanel />
      </div>

      {/* Footer */}
      <div className="border-t border-sidebar-border px-4 py-3">
        <p className="text-xs text-muted-foreground">Crosslink Dashboard</p>
        <p className="text-xs text-muted-foreground/60">v0.1.0</p>
      </div>
    </aside>
  );
}
