// Smoke coverage for the Terminals page. Mocks the API hooks so we
// can assert spawn-form behaviour without a real PTY broker.

import "@testing-library/jest-dom/vitest";
import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const stubMutation = () => ({
  mutate: vi.fn(),
  mutateAsync: vi.fn(),
  reset: vi.fn(),
  isPending: false,
  isSuccess: false,
  isError: false,
  error: null as Error | null,
  data: undefined,
});

const stubQuery = <T,>(data: T) => ({
  data,
  isLoading: false,
  isError: false,
  error: null as Error | null,
  refetch: vi.fn(),
});

const mocks = {
  useProjects: vi.fn(),
  usePtySessions: vi.fn(),
  useSpawnPty: vi.fn(),
};

vi.mock("@/api/client", async () => {
  const actual = await vi.importActual<typeof import("@/api/client")>("@/api/client");
  return {
    ...actual,
    useProjects: () => mocks.useProjects(),
    usePtySessions: () => mocks.usePtySessions(),
    useSpawnPty: () => mocks.useSpawnPty(),
  };
});

// xterm.js needs DOM APIs jsdom doesn't provide; bypass the terminal
// component so the page test stays focused on the spawn / list flows.
vi.mock("@/components/PtyTerminal", () => ({
  PtyTerminal: ({ sessionId }: { sessionId: string }) => (
    <div data-testid="terminal-stub">{sessionId}</div>
  ),
}));

import type { ProjectListItem, PtySession } from "@/api/types";

function withClient(ui: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return <QueryClientProvider client={client}>{ui}</QueryClientProvider>;
}

const project: ProjectListItem = {
  slug: "owner/repo",
  status: "active",
  pinned: false,
  hub_sha: null,
  hub_fetched_at: null,
  last_activity_at: null,
  added_at: "2026-04-20T00:00:00Z",
  write_capability: "ready",
  counters: {
    open_issues: 0,
    overdue_issues: 0,
    due_soon_issues: 0,
    blocked_issues: 0,
    active_agents: 0,
    stale_locks: 0,
    ci_status: null,
    updated_at: null,
  },
};

const session: PtySession = {
  id: "pty-abc",
  project_slug: "owner/repo",
  command: "crosslink design",
  started_at: "2026-04-20T18:00:00Z",
  exit_code: null,
};

beforeEach(() => {
  mocks.useProjects.mockReturnValue(stubQuery([project]));
  mocks.usePtySessions.mockReturnValue(stubQuery([session]));
  mocks.useSpawnPty.mockReturnValue(stubMutation());
});

describe("Terminals page", () => {
  it("lists existing sessions with attach button", async () => {
    const { Terminals } = await import("../Terminals");
    render(withClient(<Terminals />));

    expect(screen.getByText("crosslink design")).toBeInTheDocument();
    expect(screen.getByText(/in owner\/repo/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /attach/i })).toBeInTheDocument();
  });

  it("spawn form posts the chosen project + command + args", async () => {
    const spawn = stubMutation();
    mocks.useSpawnPty.mockReturnValue(spawn);

    const { Terminals } = await import("../Terminals");
    render(withClient(<Terminals />));

    fireEvent.change(screen.getByRole("combobox"), { target: { value: "owner/repo" } });
    fireEvent.change(screen.getByPlaceholderText(/crosslink design/i), {
      target: { value: "crosslink kickoff run" },
    });
    fireEvent.change(screen.getByPlaceholderText(/space-separated/i), {
      target: { value: "--plan plan.md" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^spawn$/i }));

    expect(spawn.mutate).toHaveBeenCalledWith(
      {
        project_slug: "owner/repo",
        command: "crosslink kickoff run",
        args: ["--plan", "plan.md"],
      },
      expect.any(Object),
    );
  });

  it("attach button switches active session", async () => {
    const { Terminals } = await import("../Terminals");
    render(withClient(<Terminals />));

    fireEvent.click(screen.getByRole("button", { name: /attach/i }));
    expect(screen.getByTestId("terminal-stub")).toHaveTextContent("pty-abc");
  });

  it("shortcut buttons populate the command field", async () => {
    const { Terminals } = await import("../Terminals");
    render(withClient(<Terminals />));

    fireEvent.click(screen.getByRole("button", { name: "shell" }));
    expect(screen.getByPlaceholderText(/crosslink design/i)).toHaveValue("/bin/bash");
  });
});
