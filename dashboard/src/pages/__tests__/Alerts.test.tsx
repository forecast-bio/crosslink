// Coverage for the /alerts page — specifically the per-row
// expand/collapse behaviour and the kind-specific action bar.

import "@testing-library/jest-dom/vitest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import type { AlertItem } from "@/api/types";
import { parseSubjectRef } from "../Alerts";

const stubMutation = <TData, TVars>() => ({
  mutate: vi.fn<(vars: TVars) => void>(),
  mutateAsync: vi.fn(),
  reset: vi.fn(),
  isPending: false,
  isSuccess: false,
  isError: false,
  error: null as Error | null,
  data: undefined as TData | undefined,
});

const stubQuery = <T,>(
  data: T,
  overrides: Partial<{ isLoading: boolean; error: Error | null }> = {},
) => ({
  data,
  isLoading: false,
  isFetching: false,
  isError: false,
  error: null,
  refetch: vi.fn(),
  ...overrides,
});

const mocks = {
  useAlerts: vi.fn(),
  useCloseIssue: vi.fn(),
  useCommentIssue: vi.fn(),
  useReleaseLock: vi.fn(),
  useStealLock: vi.fn(),
  useAgentRequest: vi.fn(),
};

vi.mock("@/api/client", async () => {
  const actual = await vi.importActual<typeof import("@/api/client")>(
    "@/api/client",
  );
  return {
    ...actual,
    useAlerts: () => mocks.useAlerts(),
    useCloseIssue: () => mocks.useCloseIssue(),
    useCommentIssue: () => mocks.useCommentIssue(),
    useReleaseLock: () => mocks.useReleaseLock(),
    useStealLock: () => mocks.useStealLock(),
    useAgentRequest: () => mocks.useAgentRequest(),
  };
});

function render_(ui: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>{ui}</MemoryRouter>
    </QueryClientProvider>,
  );
}

function mkAlert(overrides: Partial<AlertItem>): AlertItem {
  return {
    id: 1,
    project_slug: "owner/repo",
    kind: "stale_lock",
    severity: "warning",
    subject_ref: "lock:42",
    detail: "Lock held > 60 min",
    opened_at: "2026-04-21T12:00:00Z",
    resolved_at: null,
    acknowledged_at: null,
    ...overrides,
  };
}

describe("parseSubjectRef", () => {
  it("splits on the first colon", () => {
    expect(parseSubjectRef("lock:42")).toEqual({ kind: "lock", id: "42" });
    expect(parseSubjectRef("issue:#17")).toEqual({ kind: "issue", id: "#17" });
    expect(parseSubjectRef("commit:abc:def")).toEqual({
      kind: "commit",
      id: "abc:def",
    });
  });

  it("returns null for missing / malformed input", () => {
    expect(parseSubjectRef(null)).toBeNull();
    expect(parseSubjectRef("")).toBeNull();
    expect(parseSubjectRef("no-colon")).toBeNull();
    expect(parseSubjectRef(":leading")).toBeNull();
    expect(parseSubjectRef("trailing:")).toBeNull();
  });
});

describe("Alerts page", () => {
  beforeEach(() => {
    mocks.useAlerts.mockReturnValue(stubQuery<AlertItem[] | undefined>([]));
    mocks.useCloseIssue.mockReturnValue(stubMutation());
    mocks.useCommentIssue.mockReturnValue(stubMutation());
    mocks.useReleaseLock.mockReturnValue(stubMutation());
    mocks.useStealLock.mockReturnValue(stubMutation());
    mocks.useAgentRequest.mockReturnValue(stubMutation());
  });

  it("renders 'all clear' when no alerts", async () => {
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);
    expect(screen.getByText(/all clear/i)).toBeInTheDocument();
  });

  it("renders loading state", async () => {
    mocks.useAlerts.mockReturnValue(
      stubQuery<AlertItem[] | undefined>(undefined, { isLoading: true }),
    );
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);
    expect(screen.getByText(/loading alerts/i)).toBeInTheDocument();
  });

  it("surfaces load error", async () => {
    mocks.useAlerts.mockReturnValue(
      stubQuery<AlertItem[] | undefined>(undefined, {
        error: new Error("upstream gone"),
      }),
    );
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);
    expect(screen.getByText(/upstream gone/i)).toBeInTheDocument();
  });

  it("starts collapsed and expands on click", async () => {
    mocks.useAlerts.mockReturnValue(stubQuery([mkAlert({})]));
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    const toggle = screen.getByRole("button", { name: /toggle stale_lock/i });
    expect(toggle).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    // After expand, action buttons are visible.
    expect(
      screen.getByRole("button", { name: /release lock/i }),
    ).toBeInTheDocument();
  });

  it("Enter key toggles expansion", async () => {
    mocks.useAlerts.mockReturnValue(stubQuery([mkAlert({})]));
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    const toggle = screen.getByRole("button", { name: /toggle stale_lock/i });
    fireEvent.keyDown(toggle, { key: "Enter" });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    fireEvent.keyDown(toggle, { key: " " });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
  });

  it("lock subject shows Release + Steal wired to hooks", async () => {
    const release = stubMutation<unknown, number>();
    const steal = stubMutation<unknown, number>();
    mocks.useReleaseLock.mockReturnValue(release);
    mocks.useStealLock.mockReturnValue(steal);
    mocks.useAlerts.mockReturnValue(
      stubQuery([mkAlert({ subject_ref: "lock:42" })]),
    );
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    fireEvent.click(screen.getByRole("button", { name: /toggle stale_lock/i }));
    fireEvent.click(screen.getByRole("button", { name: /release lock/i }));
    expect(release.mutate).toHaveBeenCalledWith(42);

    fireEvent.click(screen.getByRole("button", { name: /steal lock/i }));
    expect(steal.mutate).toHaveBeenCalledWith(42);
  });

  it("issue subject shows Close + Comment; Close calls useCloseIssue", async () => {
    const close = stubMutation<unknown, number>();
    mocks.useCloseIssue.mockReturnValue(close);
    mocks.useAlerts.mockReturnValue(
      stubQuery([
        mkAlert({
          kind: "overdue_issue",
          subject_ref: "issue:17",
          severity: "warning",
        }),
      ]),
    );
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    fireEvent.click(
      screen.getByRole("button", { name: /toggle overdue_issue/i }),
    );
    fireEvent.click(screen.getByRole("button", { name: /close issue/i }));
    expect(close.mutate).toHaveBeenCalledWith(17);
  });

  it("comment drawer posts via useCommentIssue", async () => {
    const comment = stubMutation<unknown, { issueId: number; content: string }>();
    mocks.useCommentIssue.mockReturnValue(comment);
    mocks.useAlerts.mockReturnValue(
      stubQuery([
        mkAlert({ kind: "overdue_issue", subject_ref: "issue:17" }),
      ]),
    );
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    fireEvent.click(
      screen.getByRole("button", { name: /toggle overdue_issue/i }),
    );
    fireEvent.click(screen.getByRole("button", { name: /^comment$/i }));
    fireEvent.change(screen.getByPlaceholderText(/comment text/i), {
      target: { value: "bumping this" },
    });
    fireEvent.click(screen.getByRole("button", { name: /post comment/i }));

    expect(comment.mutate).toHaveBeenCalledWith(
      { issueId: 17, content: "bumping this" },
      expect.any(Object),
    );
  });

  it("agent subject shows pause/resume/kill", async () => {
    const req = stubMutation<
      unknown,
      { agentId: string; kind: "pause" | "resume" | "kill" | "reprioritise" }
    >();
    mocks.useAgentRequest.mockReturnValue(req);
    mocks.useAlerts.mockReturnValue(
      stubQuery([
        mkAlert({
          kind: "silent_agent",
          subject_ref: "agent:jus4",
          severity: "critical",
        }),
      ]),
    );
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    fireEvent.click(
      screen.getByRole("button", { name: /toggle silent_agent/i }),
    );
    fireEvent.click(screen.getByRole("button", { name: /pause agent/i }));
    expect(req.mutate).toHaveBeenCalledWith({
      agentId: "jus4",
      kind: "pause",
    });
  });

  it("subject-less alert shows only the project link", async () => {
    mocks.useAlerts.mockReturnValue(
      stubQuery([
        mkAlert({
          kind: "unreachable_project",
          subject_ref: null,
          detail: "could not fetch",
        }),
      ]),
    );
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    fireEvent.click(
      screen.getByRole("button", { name: /toggle unreachable_project/i }),
    );
    expect(
      screen.queryByRole("button", { name: /release lock/i }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /close issue/i }),
    ).not.toBeInTheDocument();
    // Project link is present in both summary and expanded body.
    expect(screen.getAllByRole("link", { name: /open project/i }).length).toBe(
      1,
    );
  });

  it("non-numeric issue id hides action buttons (local-only issue)", async () => {
    mocks.useAlerts.mockReturnValue(
      stubQuery([
        mkAlert({
          kind: "overdue_issue",
          subject_ref: "issue:bd1f0caf-1234",
        }),
      ]),
    );
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    fireEvent.click(
      screen.getByRole("button", { name: /toggle overdue_issue/i }),
    );
    expect(
      screen.queryByRole("button", { name: /close issue/i }),
    ).not.toBeInTheDocument();
  });

  it("clicking the project link does not toggle expansion", async () => {
    mocks.useAlerts.mockReturnValue(stubQuery([mkAlert({})]));
    const { Alerts } = await import("../Alerts");
    render_(<Alerts />);

    const toggle = screen.getByRole("button", { name: /toggle stale_lock/i });
    const projectLink = screen.getByRole("link", { name: "owner/repo" });
    fireEvent.click(projectLink);
    // Link nav doesn't bubble up, so still collapsed.
    expect(toggle).toHaveAttribute("aria-expanded", "false");
  });
});
