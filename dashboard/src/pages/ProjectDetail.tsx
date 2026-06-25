// Drill-down view for a single tracked project. Pulls the full
// `ProjectDetail` payload from `/api/v1/dashboard/projects/{*slug}` and
// renders sections for issues, agents, and locks. Includes the full
// P1.8–P1.9 write surface: close / reopen / comment / block / unblock /
// relate / label / unlabel on issues, plus create-milestone.

import { useState } from "react";
import { Link, useParams } from "react-router-dom";

import {
  useAgentRequest,
  useBlockIssue,
  useCloseIssue,
  useCommentIssue,
  useCreateMilestone,
  useInitProject,
  useLabelIssue,
  useProject,
  useRelateIssue,
  useReleaseLock,
  useReopenIssue,
  useStealLock,
  useUnblockIssue,
  useUnlabelIssue,
} from "@/api/client";
import type {
  AgentRequestKind,
  AgentRequestsForAgent,
  IssueFile,
  LockEntry,
} from "@/api/types";

export function ProjectDetail() {
  // React Router wildcards ({*slug}) are surfaced via the `"*"` param key.
  const { "*": slug } = useParams();
  const { data, isLoading, error } = useProject(slug ?? null);

  if (!slug) {
    return <FallbackMessage tone="error">Missing project slug in URL.</FallbackMessage>;
  }
  if (isLoading) {
    return <FallbackMessage tone="info">Loading {slug}…</FallbackMessage>;
  }
  if (error) {
    return (
      <FallbackMessage tone="error">
        Failed to load {slug}: {error.message}
      </FallbackMessage>
    );
  }
  if (!data) {
    return <FallbackMessage tone="info">No data for {slug}.</FallbackMessage>;
  }

  const openIssues = data.issues.filter((i) => i.status === "open");
  const closedIssues = data.issues.filter((i) => i.status === "closed");

  return (
    <main className="mx-auto max-w-6xl px-6 py-6">
      <nav className="mb-4 text-sm">
        <Link to="/" className="text-muted-foreground hover:underline">
          ← All projects
        </Link>
      </nav>
      <header className="mb-6 flex items-baseline justify-between">
        <h1 className="text-2xl font-semibold">{data.slug}</h1>
        <span className="text-xs text-muted-foreground">
          layout v{data.layout_version}
          {data.hub_sha && ` · ${data.hub_sha.slice(0, 7)}`}
        </span>
      </header>

      {data.write_capability !== "ready" && (
        <InitBanner slug={data.slug} capability={data.write_capability} />
      )}

      <section className="mb-6 grid grid-cols-2 gap-3 sm:grid-cols-4 lg:grid-cols-6">
        <Counter label="Open" value={data.counters.open_issues} />
        <Counter label="Overdue" value={data.counters.overdue_issues} warn={data.counters.overdue_issues > 0} />
        <Counter label="Due soon" value={data.counters.due_soon_issues} />
        <Counter label="Blocked" value={data.counters.blocked_issues} warn={data.counters.blocked_issues > 0} />
        <Counter label="Agents" value={data.counters.active_agents} />
        <Counter label="Stale locks" value={data.counters.stale_locks} warn={data.counters.stale_locks > 0} />
      </section>

      <section className="mb-8">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Agents ({data.agents.length})
        </h2>
        {data.agents.length === 0 && data.agent_requests.length === 0 ? (
          <p className="text-sm text-muted-foreground">No agents have heartbeated on this project.</p>
        ) : (
          <ul className="divide-y divide-border rounded border bg-card">
            {agentRows(data.agents, data.agent_requests).map((row) => (
              <AgentRow
                key={row.agent_id}
                slug={data.slug}
                agentId={row.agent_id}
                lastHeartbeat={row.last_heartbeat}
                requests={row.requests}
              />
            ))}
          </ul>
        )}
      </section>

      <section className="mb-8">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Locks ({data.locks.length})
        </h2>
        {data.locks.length === 0 ? (
          <p className="text-sm text-muted-foreground">No active locks.</p>
        ) : (
          <ul className="divide-y divide-border rounded border bg-card">
            {data.locks.map((l) => (
              <LockRow key={l.issue_id} slug={data.slug} lock={l} />
            ))}
          </ul>
        )}
      </section>

      <section className="mb-6">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Milestones
        </h2>
        <NewMilestoneForm slug={data.slug} />
      </section>

      <section>
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Issues ({openIssues.length} open, {closedIssues.length} closed)
        </h2>
        {openIssues.length === 0 ? (
          <p className="text-sm text-muted-foreground">No open issues.</p>
        ) : (
          <ul className="divide-y divide-border rounded border bg-card">
            {openIssues.map((i) => (
              <OpenIssueRow key={i.uuid} slug={data.slug} issue={i} />
            ))}
          </ul>
        )}
        {closedIssues.length > 0 && (
          <details className="mt-4">
            <summary className="cursor-pointer text-xs uppercase tracking-wide text-muted-foreground">
              Closed issues ({closedIssues.length})
            </summary>
            <ul className="mt-2 divide-y divide-border rounded border bg-card">
              {closedIssues.map((i) => (
                <ClosedIssueRow key={i.uuid} slug={data.slug} issue={i} />
              ))}
            </ul>
          </details>
        )}
      </section>
    </main>
  );
}

/// Top-of-page banner shown when the tracked workspace isn't
/// fully initialized — writes will fail otherwise. Surfaces an
/// inline "Initialize" form with an agent-id input that shells the
/// backend retrofit endpoint (`POST /w/{slug}/init`) and invalidates
/// the project detail query on success so the banner disappears.
function InitBanner({
  slug,
  capability,
}: {
  slug: string;
  capability: "not_initialized" | "agent_missing";
}) {
  const [agentId, setAgentId] = useState("");
  const init = useInitProject(slug);
  const reason =
    capability === "not_initialized"
      ? "crosslink init hasn't been run in this workspace"
      : "crosslink agent init hasn't been run in this workspace";

  return (
    <section
      className="mb-6 rounded border border-amber-500/60 bg-amber-500/10 p-4 text-sm"
      role="status"
      aria-label="workspace initialization required"
    >
      <p className="mb-2 font-semibold text-amber-500">
        ⚠ This workspace isn't initialized for dashboard writes.
      </p>
      <p className="mb-3 text-amber-500/80">
        {reason}. Close / release / comment / lock-control actions will
        fail until <code className="font-mono">crosslink init</code> and{" "}
        <code className="font-mono">crosslink agent init &lt;id&gt;</code>{" "}
        both run here. You can do that from a shell, or kick it off
        below — the dashboard binary will run both commands in this
        clone on your behalf.
      </p>
      <form
        className="flex flex-wrap items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          const id = agentId.trim();
          if (!id) return;
          init.mutate({ agentId: id });
        }}
      >
        <input
          value={agentId}
          onChange={(e) => setAgentId(e.target.value)}
          placeholder="agent id (alphanumeric, hyphens, underscores)"
          className="flex-1 min-w-[16rem] rounded border bg-background px-2 py-1 font-mono text-xs"
          aria-label="Agent ID"
        />
        <button
          type="submit"
          disabled={!agentId.trim() || init.isPending}
          className="rounded border border-amber-500/70 bg-amber-500/20 px-2 py-1 text-xs font-medium text-amber-600 hover:bg-amber-500/30 disabled:opacity-50"
        >
          {init.isPending ? "Initializing…" : "Initialize"}
        </button>
        {init.error && (
          <span className="text-xs text-rose-500">{init.error.message}</span>
        )}
        {init.isSuccess && (
          <span className="text-xs text-emerald-500">
            Initialized — banner clears on the next poll tick.
          </span>
        )}
      </form>
    </section>
  );
}

export function OpenIssueRow({ slug, issue }: { slug: string; issue: IssueFile }) {
  const [commentOpen, setCommentOpen] = useState(false);
  const [commentText, setCommentText] = useState("");
  const [moreOpen, setMoreOpen] = useState(false);
  const [labelText, setLabelText] = useState("");
  const [blockerText, setBlockerText] = useState("");
  const [unblockText, setUnblockText] = useState("");
  const [relateText, setRelateText] = useState("");

  const close = useCloseIssue(slug);
  const comment = useCommentIssue(slug);
  const label = useLabelIssue(slug);
  const unlabel = useUnlabelIssue(slug);
  const block = useBlockIssue(slug);
  const unblock = useUnblockIssue(slug);
  const relate = useRelateIssue(slug);

  const id = issue.display_id;
  const canAct = id != null;

  const moreError =
    label.error?.message ??
    unlabel.error?.message ??
    block.error?.message ??
    unblock.error?.message ??
    relate.error?.message;

  return (
    <li className="px-3 py-2 text-sm">
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="text-muted-foreground tabular-nums">
          {id != null ? `#${id}` : "—"}
        </span>
        <span className="font-medium">{issue.title}</span>
        <span className="text-xs text-muted-foreground">[{issue.priority}]</span>
        {issue.due_at && (
          <span className="text-xs text-muted-foreground">due {issue.due_at}</span>
        )}
        {(issue.blockers?.length ?? 0) > 0 && (
          <span className="text-xs text-amber-500">
            blocked by {issue.blockers?.length ?? 0}
          </span>
        )}
        <span className="ml-auto flex items-center gap-2">
          <button
            type="button"
            disabled={!canAct || close.isPending}
            onClick={() => (id != null ? close.mutate(id) : undefined)}
            className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
          >
            {close.isPending ? "Closing…" : "Close"}
          </button>
          <button
            type="button"
            disabled={!canAct}
            onClick={() => setCommentOpen((v) => !v)}
            className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
          >
            {commentOpen ? "Cancel" : "Comment"}
          </button>
          <button
            type="button"
            disabled={!canAct}
            onClick={() => setMoreOpen((v) => !v)}
            className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
          >
            {moreOpen ? "Hide" : "More"}
          </button>
        </span>
      </div>
      {(issue.labels?.length ?? 0) > 0 && (
        <div className="mt-1 flex flex-wrap gap-1">
          {(issue.labels ?? []).map((l) => (
            <span
              key={l}
              className="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] text-muted-foreground"
            >
              {l}
              {canAct && id != null && (
                <button
                  type="button"
                  disabled={unlabel.isPending}
                  onClick={() =>
                    unlabel.mutate({ issueId: id, label: l })
                  }
                  className="text-muted-foreground hover:text-rose-500 disabled:opacity-50"
                  aria-label={`Remove label ${l}`}
                >
                  ×
                </button>
              )}
            </span>
          ))}
        </div>
      )}
      {close.error && (
        <p className="mt-1 text-xs text-rose-500">{close.error.message}</p>
      )}
      {commentOpen && canAct && id != null && (
        <form
          className="mt-2 flex flex-col gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (!commentText.trim()) return;
            comment.mutate(
              { issueId: id, content: commentText },
              {
                onSuccess: () => {
                  setCommentText("");
                  setCommentOpen(false);
                },
              },
            );
          }}
        >
          <textarea
            value={commentText}
            onChange={(e) => setCommentText(e.target.value)}
            placeholder="Comment text"
            rows={3}
            className="w-full rounded border bg-background p-2 text-sm"
          />
          <div className="flex items-center gap-2">
            <button
              type="submit"
              disabled={!commentText.trim() || comment.isPending}
              className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
            >
              {comment.isPending ? "Posting…" : "Post comment"}
            </button>
            {comment.error && (
              <span className="text-xs text-rose-500">{comment.error.message}</span>
            )}
          </div>
        </form>
      )}
      {moreOpen && canAct && id != null && (
        <div className="mt-2 flex flex-col gap-2 rounded border bg-background/50 p-2">
          <form
            className="flex items-center gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              const v = labelText.trim();
              if (!v) return;
              label.mutate(
                { issueId: id, label: v },
                { onSuccess: () => setLabelText("") },
              );
            }}
          >
            <label className="text-xs text-muted-foreground w-16">Label</label>
            <input
              value={labelText}
              onChange={(e) => setLabelText(e.target.value)}
              placeholder="label-name"
              className="flex-1 rounded border bg-background px-2 py-0.5 text-xs"
            />
            <button
              type="submit"
              disabled={!labelText.trim() || label.isPending}
              className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
            >
              {label.isPending ? "Adding…" : "Add"}
            </button>
          </form>
          <form
            className="flex items-center gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              const n = Number(blockerText.trim());
              if (!Number.isInteger(n) || n <= 0) return;
              block.mutate(
                { issueId: id, blockerId: n },
                { onSuccess: () => setBlockerText("") },
              );
            }}
          >
            <label className="text-xs text-muted-foreground w-16">
              Blocked by
            </label>
            <input
              type="number"
              min={1}
              value={blockerText}
              onChange={(e) => setBlockerText(e.target.value)}
              placeholder="issue id"
              className="flex-1 rounded border bg-background px-2 py-0.5 text-xs"
            />
            <button
              type="submit"
              disabled={!blockerText.trim() || block.isPending}
              className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
            >
              {block.isPending ? "Adding…" : "Block"}
            </button>
          </form>
          <form
            className="flex items-center gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              const n = Number(unblockText.trim());
              if (!Number.isInteger(n) || n <= 0) return;
              unblock.mutate(
                { issueId: id, blockerId: n },
                { onSuccess: () => setUnblockText("") },
              );
            }}
          >
            <label className="text-xs text-muted-foreground w-16">Unblock</label>
            <input
              type="number"
              min={1}
              value={unblockText}
              onChange={(e) => setUnblockText(e.target.value)}
              placeholder="issue id"
              className="flex-1 rounded border bg-background px-2 py-0.5 text-xs"
            />
            <button
              type="submit"
              disabled={!unblockText.trim() || unblock.isPending}
              className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
            >
              {unblock.isPending ? "Clearing…" : "Clear"}
            </button>
          </form>
          <form
            className="flex items-center gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              const n = Number(relateText.trim());
              if (!Number.isInteger(n) || n <= 0) return;
              relate.mutate(
                { issueId: id, otherId: n },
                { onSuccess: () => setRelateText("") },
              );
            }}
          >
            <label className="text-xs text-muted-foreground w-16">
              Related
            </label>
            <input
              type="number"
              min={1}
              value={relateText}
              onChange={(e) => setRelateText(e.target.value)}
              placeholder="issue id"
              className="flex-1 rounded border bg-background px-2 py-0.5 text-xs"
            />
            <button
              type="submit"
              disabled={!relateText.trim() || relate.isPending}
              className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
            >
              {relate.isPending ? "Linking…" : "Link"}
            </button>
          </form>
          {moreError && (
            <p className="text-xs text-rose-500">{moreError}</p>
          )}
        </div>
      )}
    </li>
  );
}

export function NewMilestoneForm({ slug }: { slug: string }) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [expanded, setExpanded] = useState(false);
  const create = useCreateMilestone(slug);

  if (!expanded) {
    return (
      <button
        type="button"
        onClick={() => setExpanded(true)}
        className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10"
      >
        + New milestone
      </button>
    );
  }

  return (
    <form
      className="flex flex-col gap-2 rounded border bg-card p-3"
      onSubmit={(e) => {
        e.preventDefault();
        if (!name.trim()) return;
        create.mutate(
          {
            name: name.trim(),
            description: description.trim() || undefined,
          },
          {
            onSuccess: () => {
              setName("");
              setDescription("");
              setExpanded(false);
            },
          },
        );
      }}
    >
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder="Milestone name"
        className="rounded border bg-background px-2 py-1 text-sm"
      />
      <textarea
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        placeholder="Description (optional)"
        rows={2}
        className="rounded border bg-background p-2 text-sm"
      />
      <div className="flex items-center gap-2">
        <button
          type="submit"
          disabled={!name.trim() || create.isPending}
          className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
        >
          {create.isPending ? "Creating…" : "Create"}
        </button>
        <button
          type="button"
          onClick={() => {
            setExpanded(false);
            setName("");
            setDescription("");
          }}
          className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10"
        >
          Cancel
        </button>
        {create.error && (
          <span className="text-xs text-rose-500">{create.error.message}</span>
        )}
      </div>
    </form>
  );
}

export function ClosedIssueRow({ slug, issue }: { slug: string; issue: IssueFile }) {
  const reopen = useReopenIssue(slug);
  const id = issue.display_id;
  const canAct = id != null;
  return (
    <li className="px-3 py-2 text-sm">
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="text-muted-foreground tabular-nums">
          {id != null ? `#${id}` : "—"}
        </span>
        <span className="font-medium opacity-70 line-through">{issue.title}</span>
        <span className="ml-auto">
          <button
            type="button"
            disabled={!canAct || reopen.isPending}
            onClick={() => (id != null ? reopen.mutate(id) : undefined)}
            className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
          >
            {reopen.isPending ? "Reopening…" : "Reopen"}
          </button>
        </span>
      </div>
      {reopen.error && (
        <p className="mt-1 text-xs text-rose-500">{reopen.error.message}</p>
      )}
    </li>
  );
}

type AgentRowData = {
  agent_id: string;
  last_heartbeat: string | null;
  requests: AgentRequestsForAgent["requests"];
};

/// Merge heartbeats + request streams into a single ordered list.
/// Every agent with either a heartbeat or a request shows up exactly
/// once; heartbeat-less agents (request-only targets) render with a
/// placeholder last-heartbeat.
function agentRows(
  agents: { agent_id: string; last_heartbeat: string }[],
  requestGroups: AgentRequestsForAgent[],
): AgentRowData[] {
  const byId = new Map<string, AgentRowData>();
  for (const a of agents) {
    byId.set(a.agent_id, {
      agent_id: a.agent_id,
      last_heartbeat: a.last_heartbeat,
      requests: [],
    });
  }
  for (const g of requestGroups) {
    const existing = byId.get(g.agent_id);
    if (existing) {
      existing.requests = g.requests;
    } else {
      byId.set(g.agent_id, {
        agent_id: g.agent_id,
        last_heartbeat: null,
        requests: g.requests,
      });
    }
  }
  return Array.from(byId.values()).sort((a, b) =>
    a.agent_id.localeCompare(b.agent_id),
  );
}

export function AgentRow({
  slug,
  agentId,
  lastHeartbeat,
  requests,
}: {
  slug: string;
  agentId: string;
  lastHeartbeat: string | null;
  requests: AgentRequestsForAgent["requests"];
}) {
  const [formOpen, setFormOpen] = useState(false);
  const [kind, setKind] = useState<AgentRequestKind>("pause");
  const [subjectIssue, setSubjectIssue] = useState("");
  const [reason, setReason] = useState("");
  const send = useAgentRequest(slug);

  const pendingCount = requests.filter((r) => r.ack === null).length;

  return (
    <li className="px-3 py-2 text-sm">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <span className="flex items-baseline gap-2">
          <span className="font-medium">{agentId}</span>
          {pendingCount > 0 && (
            <span className="rounded-full bg-amber-500/20 px-2 py-0.5 text-[10px] text-amber-500">
              {pendingCount} pending request{pendingCount === 1 ? "" : "s"}
            </span>
          )}
        </span>
        <span className="flex items-center gap-2">
          <span className="text-xs text-muted-foreground tabular-nums">
            {lastHeartbeat
              ? `last heartbeat ${lastHeartbeat}`
              : "no heartbeat"}
          </span>
          <button
            type="button"
            onClick={() => setFormOpen((v) => !v)}
            className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10"
          >
            {formOpen ? "Cancel" : "Send request"}
          </button>
        </span>
      </div>

      {formOpen && (
        <form
          className="mt-2 flex flex-col gap-2 rounded border bg-background/50 p-2"
          onSubmit={(e) => {
            e.preventDefault();
            const n = subjectIssue.trim()
              ? Number(subjectIssue.trim())
              : undefined;
            send.mutate(
              {
                agentId,
                kind,
                subjectIssue:
                  Number.isInteger(n) && (n as number) > 0
                    ? (n as number)
                    : undefined,
                reason: reason.trim() || undefined,
              },
              {
                onSuccess: () => {
                  setFormOpen(false);
                  setSubjectIssue("");
                  setReason("");
                },
              },
            );
          }}
        >
          <div className="flex items-center gap-2">
            <label className="text-xs text-muted-foreground w-16">Kind</label>
            <select
              value={kind}
              onChange={(e) => setKind(e.target.value as AgentRequestKind)}
              className="rounded border bg-background px-2 py-0.5 text-xs"
            >
              <option value="pause">pause</option>
              <option value="resume">resume</option>
              <option value="kill">kill</option>
              <option value="reprioritise">reprioritise</option>
            </select>
          </div>
          <div className="flex items-center gap-2">
            <label className="text-xs text-muted-foreground w-16">
              Issue id
            </label>
            <input
              type="number"
              min={1}
              value={subjectIssue}
              onChange={(e) => setSubjectIssue(e.target.value)}
              placeholder="optional"
              className="flex-1 rounded border bg-background px-2 py-0.5 text-xs"
            />
          </div>
          <div className="flex items-start gap-2">
            <label className="text-xs text-muted-foreground w-16 mt-1">
              Reason
            </label>
            <textarea
              value={reason}
              onChange={(e) => setReason(e.target.value)}
              placeholder="optional — shows up in audit trail"
              rows={2}
              className="flex-1 rounded border bg-background p-1 text-xs"
            />
          </div>
          <div className="flex items-center gap-2">
            <button
              type="submit"
              disabled={send.isPending}
              className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
            >
              {send.isPending ? "Sending…" : "Send"}
            </button>
            {send.error && (
              <span className="text-xs text-rose-500">{send.error.message}</span>
            )}
          </div>
        </form>
      )}

      {requests.length > 0 && (
        <ul className="mt-2 space-y-1">
          {requests.map((r) => (
            <li
              key={r.request_id}
              className="flex flex-wrap items-baseline gap-2 border-l-2 border-border pl-2 text-xs"
            >
              <span className="tabular-nums text-muted-foreground">
                {r.request_id}
              </span>
              <span className="font-medium">{r.kind}</span>
              {r.subject_issue != null && (
                <span className="text-muted-foreground">#{r.subject_issue}</span>
              )}
              {r.ack ? (
                <span
                  className={
                    r.ack.acted ? "text-emerald-500" : "text-amber-500"
                  }
                >
                  acked: {r.ack.result}
                </span>
              ) : (
                <span className="text-amber-500">pending</span>
              )}
              {r.reason && (
                <span className="text-muted-foreground">— {r.reason}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </li>
  );
}

export function LockRow({ slug, lock }: { slug: string; lock: LockEntry }) {
  const release = useReleaseLock(slug);
  const steal = useStealLock(slug);
  const error = release.error?.message ?? steal.error?.message;

  return (
    <li className="flex flex-wrap items-baseline justify-between gap-2 px-3 py-2 text-sm">
      <span>
        #{lock.issue_id} held by <span className="font-medium">{lock.agent_id}</span>
        {lock.branch && <span className="text-muted-foreground"> · {lock.branch}</span>}
      </span>
      <span className="flex items-center gap-2">
        <span className="text-xs text-muted-foreground tabular-nums">
          claimed {lock.claimed_at}
        </span>
        <button
          type="button"
          disabled={release.isPending}
          onClick={() => release.mutate(lock.issue_id)}
          className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
        >
          {release.isPending ? "Releasing…" : "Release"}
        </button>
        <button
          type="button"
          disabled={steal.isPending}
          onClick={() => {
            if (
              window.confirm(
                `Steal lock on #${lock.issue_id} from ${lock.agent_id}? This overrides the other agent.`,
              )
            ) {
              steal.mutate(lock.issue_id);
            }
          }}
          className="rounded border border-amber-500/40 px-2 py-0.5 text-xs text-amber-500 hover:bg-amber-500/10 disabled:opacity-50"
        >
          {steal.isPending ? "Stealing…" : "Steal"}
        </button>
      </span>
      {error && (
        <p className="w-full text-xs text-rose-500">{error}</p>
      )}
    </li>
  );
}

function Counter({ label, value, warn }: { label: string; value: number; warn?: boolean }) {
  return (
    <div className="rounded border bg-card p-2 text-center">
      <div className={`text-xl font-semibold tabular-nums ${warn ? "text-rose-500" : ""}`}>
        {value}
      </div>
      <div className="text-xs uppercase tracking-wide text-muted-foreground">{label}</div>
    </div>
  );
}

function FallbackMessage({
  children,
  tone,
}: {
  children: React.ReactNode;
  tone: "info" | "error";
}) {
  return (
    <main className="mx-auto max-w-3xl px-6 py-16">
      <p className={tone === "error" ? "text-rose-500" : "text-muted-foreground"}>{children}</p>
    </main>
  );
}
