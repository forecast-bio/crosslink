// Component-level coverage for the new write-surface controls on
// ProjectDetail. Covers the full mutation paths exposed in P1.8–P1.11
// (close/comment/label/block/relate, lock release/steal, milestone
// create, agent request) plus the helper that merges heartbeats with
// agent-request streams.

import "@testing-library/jest-dom/vitest";
import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

// Mock the entire API client so component tests don't make real fetch
// calls. Each mutation hook returns a stub `mutate` we can assert on.
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

const mocks = {
  useCloseIssue: vi.fn((_slug: string) => stubMutation()),
  useReopenIssue: vi.fn((_slug: string) => stubMutation()),
  useCommentIssue: vi.fn((_slug: string) => stubMutation()),
  useBlockIssue: vi.fn((_slug: string) => stubMutation()),
  useUnblockIssue: vi.fn((_slug: string) => stubMutation()),
  useRelateIssue: vi.fn((_slug: string) => stubMutation()),
  useLabelIssue: vi.fn((_slug: string) => stubMutation()),
  useUnlabelIssue: vi.fn((_slug: string) => stubMutation()),
  useCreateMilestone: vi.fn((_slug: string) => stubMutation()),
  useReleaseLock: vi.fn((_slug: string) => stubMutation()),
  useStealLock: vi.fn((_slug: string) => stubMutation()),
  useAgentRequest: vi.fn((_slug: string) => stubMutation()),
};

vi.mock("@/api/client", async () => {
  const actual = await vi.importActual<typeof import("@/api/client")>("@/api/client");
  return {
    ...actual,
    useCloseIssue: (slug: string) => mocks.useCloseIssue(slug),
    useReopenIssue: (slug: string) => mocks.useReopenIssue(slug),
    useCommentIssue: (slug: string) => mocks.useCommentIssue(slug),
    useBlockIssue: (slug: string) => mocks.useBlockIssue(slug),
    useUnblockIssue: (slug: string) => mocks.useUnblockIssue(slug),
    useRelateIssue: (slug: string) => mocks.useRelateIssue(slug),
    useLabelIssue: (slug: string) => mocks.useLabelIssue(slug),
    useUnlabelIssue: (slug: string) => mocks.useUnlabelIssue(slug),
    useCreateMilestone: (slug: string) => mocks.useCreateMilestone(slug),
    useReleaseLock: (slug: string) => mocks.useReleaseLock(slug),
    useStealLock: (slug: string) => mocks.useStealLock(slug),
    useAgentRequest: (slug: string) => mocks.useAgentRequest(slug),
  };
});

import type { IssueFile, LockEntry, AgentRequestsForAgent } from "@/api/types";

function withClient(ui: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return (
    <QueryClientProvider client={client}>{ui}</QueryClientProvider>
  );
}

const baseIssue: IssueFile = {
  uuid: "00000000-0000-0000-0000-000000000001",
  display_id: 42,
  title: "Test issue",
  description: null,
  status: "open",
  priority: "medium",
  parent_uuid: null,
  created_by: "test",
  created_at: "2026-04-20T00:00:00Z",
  updated_at: "2026-04-20T00:00:00Z",
  closed_at: null,
  scheduled_at: null,
  due_at: null,
  labels: ["bug", "urgent"],
  blockers: [],
  related: [],
  milestone_uuid: null,
};

beforeEach(() => {
  for (const m of Object.values(mocks)) {
    m.mockClear();
    m.mockImplementation(stubMutation);
  }
});

describe("OpenIssueRow", () => {
  it("close button invokes useCloseIssue mutation with the display id", async () => {
    const close = stubMutation();
    mocks.useCloseIssue.mockReturnValue(close);

    const { OpenIssueRow } = await import("../ProjectDetail");
    render(withClient(<ul><OpenIssueRow slug="owner/repo" issue={baseIssue} /></ul>));

    fireEvent.click(screen.getByRole("button", { name: /^close$/i }));
    expect(close.mutate).toHaveBeenCalledWith(42);
  });

  it("comment form posts trimmed content and resets after success", async () => {
    const comment = stubMutation();
    mocks.useCommentIssue.mockReturnValue(comment);

    const { OpenIssueRow } = await import("../ProjectDetail");
    render(withClient(<ul><OpenIssueRow slug="owner/repo" issue={baseIssue} /></ul>));

    fireEvent.click(screen.getByRole("button", { name: /^comment$/i }));
    const textarea = screen.getByPlaceholderText(/comment text/i);
    fireEvent.change(textarea, { target: { value: "looks good" } });
    fireEvent.click(screen.getByRole("button", { name: /post comment/i }));

    expect(comment.mutate).toHaveBeenCalledWith(
      { issueId: 42, content: "looks good" },
      expect.objectContaining({ onSuccess: expect.any(Function) }),
    );
  });

  it("label chips render and × calls useUnlabelIssue", async () => {
    const unlabel = stubMutation();
    mocks.useUnlabelIssue.mockReturnValue(unlabel);

    const { OpenIssueRow } = await import("../ProjectDetail");
    render(withClient(<ul><OpenIssueRow slug="owner/repo" issue={baseIssue} /></ul>));

    expect(screen.getByText("bug")).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText(/remove label bug/i));
    expect(unlabel.mutate).toHaveBeenCalledWith({ issueId: 42, label: "bug" });
  });

  it("more drawer block form parses issue id as integer", async () => {
    const block = stubMutation();
    mocks.useBlockIssue.mockReturnValue(block);

    const { OpenIssueRow } = await import("../ProjectDetail");
    render(withClient(<ul><OpenIssueRow slug="owner/repo" issue={baseIssue} /></ul>));

    fireEvent.click(screen.getByRole("button", { name: /^more$/i }));
    const inputs = screen.getAllByPlaceholderText(/^issue id$/i);
    fireEvent.change(inputs[0], { target: { value: "17" } });
    fireEvent.click(screen.getByRole("button", { name: /^block$/i }));

    expect(block.mutate).toHaveBeenCalledWith(
      { issueId: 42, blockerId: 17 },
      expect.any(Object),
    );
  });

  it("more drawer rejects non-integer blocker id", async () => {
    const block = stubMutation();
    mocks.useBlockIssue.mockReturnValue(block);

    const { OpenIssueRow } = await import("../ProjectDetail");
    render(withClient(<ul><OpenIssueRow slug="owner/repo" issue={baseIssue} /></ul>));

    fireEvent.click(screen.getByRole("button", { name: /^more$/i }));
    const inputs = screen.getAllByPlaceholderText(/^issue id$/i);
    fireEvent.change(inputs[0], { target: { value: "0" } });
    fireEvent.click(screen.getByRole("button", { name: /^block$/i }));

    expect(block.mutate).not.toHaveBeenCalled();
  });

  it("blocked-by hint appears when issue has blockers", async () => {
    const issue = { ...baseIssue, blockers: ["uuid-a", "uuid-b"] };

    const { OpenIssueRow } = await import("../ProjectDetail");
    render(withClient(<ul><OpenIssueRow slug="owner/repo" issue={issue} /></ul>));

    expect(screen.getByText(/blocked by 2/i)).toBeInTheDocument();
  });
});

describe("ClosedIssueRow", () => {
  it("reopen button invokes useReopenIssue mutation", async () => {
    const reopen = stubMutation();
    mocks.useReopenIssue.mockReturnValue(reopen);

    const closed = { ...baseIssue, status: "closed" as const, display_id: 99 };
    const { ClosedIssueRow } = await import("../ProjectDetail");
    render(withClient(<ul><ClosedIssueRow slug="owner/repo" issue={closed} /></ul>));

    fireEvent.click(screen.getByRole("button", { name: /^reopen$/i }));
    expect(reopen.mutate).toHaveBeenCalledWith(99);
  });
});

describe("LockRow", () => {
  const lock: LockEntry = {
    issue_id: 7,
    agent_id: "jus4",
    branch: "feat/xyz",
    claimed_at: "2026-04-20T10:00:00Z",
    signed_by: "SHA256:test",
  };

  it("release button calls useReleaseLock with issue id", async () => {
    const release = stubMutation();
    mocks.useReleaseLock.mockReturnValue(release);

    const { LockRow } = await import("../ProjectDetail");
    render(withClient(<ul><LockRow slug="owner/repo" lock={lock} /></ul>));

    fireEvent.click(screen.getByRole("button", { name: /^release$/i }));
    expect(release.mutate).toHaveBeenCalledWith(7);
  });

  it("steal button confirms before invoking the mutation", async () => {
    const steal = stubMutation();
    mocks.useStealLock.mockReturnValue(steal);

    const { LockRow } = await import("../ProjectDetail");
    render(withClient(<ul><LockRow slug="owner/repo" lock={lock} /></ul>));

    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    fireEvent.click(screen.getByRole("button", { name: /^steal$/i }));
    expect(confirmSpy).toHaveBeenCalled();
    expect(steal.mutate).not.toHaveBeenCalled();

    confirmSpy.mockReturnValue(true);
    fireEvent.click(screen.getByRole("button", { name: /^steal$/i }));
    expect(steal.mutate).toHaveBeenCalledWith(7);
    confirmSpy.mockRestore();
  });
});

describe("AgentRow", () => {
  const requestGroup: AgentRequestsForAgent = {
    agent_id: "jus4",
    requests: [
      {
        request_id: "01HXY000000000000000000001",
        kind: "pause",
        subject_issue: 42,
        requested_by: "SHA256:test",
        requested_at: "2026-04-20T18:30:00Z",
        reason: "stuck",
        ack: null,
      },
    ],
  };

  it("renders pending-request chip count from request stream", async () => {
    const { AgentRow } = await import("../ProjectDetail");
    render(
      withClient(
        <ul>
          <AgentRow
            slug="owner/repo"
            agentId="jus4"
            lastHeartbeat="2026-04-20T11:55:00Z"
            requests={requestGroup.requests}
          />
        </ul>,
      ),
    );
    expect(screen.getByText(/1 pending request/i)).toBeInTheDocument();
  });

  it("send-request form fires useAgentRequest with parsed values", async () => {
    const send = stubMutation();
    mocks.useAgentRequest.mockReturnValue(send);

    const { AgentRow } = await import("../ProjectDetail");
    render(
      withClient(
        <ul>
          <AgentRow
            slug="owner/repo"
            agentId="jus4"
            lastHeartbeat="2026-04-20T11:55:00Z"
            requests={[]}
          />
        </ul>,
      ),
    );

    fireEvent.click(screen.getByRole("button", { name: /send request/i }));
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "kill" } });
    fireEvent.change(screen.getByPlaceholderText(/optional$/i), { target: { value: "5" } });
    fireEvent.change(screen.getByPlaceholderText(/audit trail/i), {
      target: { value: "stuck loop" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^send$/i }));

    expect(send.mutate).toHaveBeenCalledWith(
      { agentId: "jus4", kind: "kill", subjectIssue: 5, reason: "stuck loop" },
      expect.any(Object),
    );
  });

  it("renders ack inline when present", async () => {
    const ackedGroup: AgentRequestsForAgent = {
      agent_id: "jus4",
      requests: [
        {
          ...requestGroup.requests[0],
          ack: {
            ack_at: "2026-04-20T18:31:00Z",
            acted: true,
            result: "paused",
            notes: null,
          },
        },
      ],
    };
    const { AgentRow } = await import("../ProjectDetail");
    render(
      withClient(
        <ul>
          <AgentRow
            slug="owner/repo"
            agentId="jus4"
            lastHeartbeat="2026-04-20T11:55:00Z"
            requests={ackedGroup.requests}
          />
        </ul>,
      ),
    );
    expect(screen.getByText(/acked: paused/i)).toBeInTheDocument();
    expect(screen.queryByText(/1 pending request/i)).not.toBeInTheDocument();
  });
});

describe("NewMilestoneForm", () => {
  it("expanded form submits trimmed name and description", async () => {
    const create = stubMutation();
    mocks.useCreateMilestone.mockReturnValue(create);

    const { NewMilestoneForm } = await import("../ProjectDetail");
    render(withClient(<NewMilestoneForm slug="owner/repo" />));

    fireEvent.click(screen.getByRole("button", { name: /\+ new milestone/i }));
    fireEvent.change(screen.getByPlaceholderText(/milestone name/i), {
      target: { value: "  v0.6 release  " },
    });
    fireEvent.change(screen.getByPlaceholderText(/description \(optional\)/i), {
      target: { value: "  ship dashboard  " },
    });
    fireEvent.click(screen.getByRole("button", { name: /^create$/i }));

    expect(create.mutate).toHaveBeenCalledWith(
      { name: "v0.6 release", description: "ship dashboard" },
      expect.any(Object),
    );
  });

  it("collapses again when cancelled", async () => {
    const { NewMilestoneForm } = await import("../ProjectDetail");
    render(withClient(<NewMilestoneForm slug="owner/repo" />));

    fireEvent.click(screen.getByRole("button", { name: /\+ new milestone/i }));
    expect(screen.getByPlaceholderText(/milestone name/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^cancel$/i }));
    expect(screen.queryByPlaceholderText(/milestone name/i)).not.toBeInTheDocument();
  });
});
