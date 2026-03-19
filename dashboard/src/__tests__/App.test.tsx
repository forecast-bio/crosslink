import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { App } from "../App";

// Mock the WebSocket client so tests don't try to open real connections
vi.mock("@/api/ws", () => ({
  wsClient: {
    connect: vi.fn(),
    disconnect: vi.fn(),
    on: vi.fn(() => vi.fn()), // returns an unsubscribe function
  },
}));

// Mock all page components — they make API calls we don't need here
vi.mock("@/pages/Dashboard", () => ({ Dashboard: () => <div data-testid="page-dashboard">Dashboard</div> }));
vi.mock("@/pages/Agents", () => ({ Agents: () => <div>Agents</div> }));
vi.mock("@/pages/AgentDetail", () => ({ AgentDetail: () => <div>AgentDetail</div> }));
vi.mock("@/pages/Issues", () => ({ Issues: () => <div>Issues</div> }));
vi.mock("@/pages/IssueDetail", () => ({ IssueDetail: () => <div>IssueDetail</div> }));
vi.mock("@/pages/Sessions", () => ({ Sessions: () => <div>Sessions</div> }));
vi.mock("@/pages/Milestones", () => ({ Milestones: () => <div>Milestones</div> }));
vi.mock("@/pages/Knowledge", () => ({ Knowledge: () => <div>Knowledge</div> }));
vi.mock("@/pages/KnowledgeDetail", () => ({ KnowledgeDetail: () => <div>KnowledgeDetail</div> }));
vi.mock("@/pages/Sync", () => ({ Sync: () => <div>Sync</div> }));
vi.mock("@/pages/Config", () => ({ Config: () => <div>Config</div> }));
vi.mock("@/pages/Orchestrator", () => ({ Orchestrator: () => <div>Orchestrator</div> }));
vi.mock("@/pages/Execution", () => ({ Execution: () => <div>Execution</div> }));
vi.mock("@/pages/Usage", () => ({ Usage: () => <div>Usage</div> }));
vi.mock("@/pages/Appearance", () => ({ Appearance: () => <div>Appearance</div> }));
vi.mock("@/components/Sidebar", () => ({ Sidebar: () => <nav data-testid="sidebar">Sidebar</nav> }));
vi.mock("@/components/CommandPalette", () => ({ CommandPalette: () => null }));
vi.mock("@/components/ThemeProvider", () => ({ ThemeProvider: () => null }));
vi.mock("@/stores/agents", () => ({
  useAgentsStore: () => ({ applyHeartbeat: vi.fn(), applyStatus: vi.fn() }),
}));
vi.mock("@/stores/issues", () => ({
  useIssuesStore: () => ({ invalidate: vi.fn() }),
}));
vi.mock("@/stores/orchestrator", () => ({
  useOrchestratorStore: () => ({ applyProgress: vi.fn() }),
}));

describe("App smoke test", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders without crashing", () => {
    const { container } = render(<App />);
    expect(container).toBeTruthy();
  });

  it("renders the sidebar", () => {
    render(<App />);
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
  });

  it("renders the default route (Dashboard page)", () => {
    render(<App />);
    expect(screen.getByTestId("page-dashboard")).toBeInTheDocument();
  });
});
