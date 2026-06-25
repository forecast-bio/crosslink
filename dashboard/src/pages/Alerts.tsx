// Full alerts page. Shows every currently-open alert across all
// tracked projects, grouped by severity then ordered by opened_at.
// Each row expands on click to reveal kind-specific actions:
//   stale_lock / silent_agent → lock controls (release / steal)
//   silent_agent              → agent control requests (pause / resume / kill)
//   overdue_issue             → issue actions (close / comment)
//   orphan_subissue           → issue actions (close)
//   unreachable_project       → link into project detail
//   ci_failure / signature_invalid → show commit SHA + project link
//
// The `subject_ref` field on each alert encodes the entity the alert
// is about (e.g. `lock:42`, `agent:jus4`, `issue:17`, `commit:abc…`,
// `project:owner/repo`) — we parse it to decide which action set to
// show. See crosslink/src/dashboard/alerts.rs for the authoritative
// list of subject_ref shapes.

import { useCallback, useState, type KeyboardEvent } from "react";
import { Link } from "react-router-dom";

import {
  useAgentRequest,
  useAlerts,
  useCloseIssue,
  useCommentIssue,
  useProject,
  useProjects,
  useReleaseLock,
  useSignBackfill,
  useStealLock,
} from "@/api/client";
import type { AlertItem, AlertSeverity, LockEntry, WriteCapability } from "@/api/types";
import { ExportMenu } from "@/components/ExportMenu";
import { groupBySeverity, SEVERITY_ORDER } from "@/lib/alerts";

const SEVERITY_CLASSES: Record<AlertSeverity, string> = {
  critical: "border-rose-500/60 bg-rose-500/10",
  warning: "border-amber-500/60 bg-amber-500/10",
  info: "border-sky-500/50 bg-sky-500/10",
};

const SEVERITY_BADGE: Record<AlertSeverity, string> = {
  critical: "bg-rose-500 text-white",
  warning: "bg-amber-500 text-white",
  info: "bg-sky-500 text-white",
};

export function Alerts() {
  const { data, isLoading, error } = useAlerts();

  if (isLoading) {
    return (
      <main className="mx-auto max-w-6xl px-6 py-8">
        <p className="text-muted-foreground">Loading alerts…</p>
      </main>
    );
  }

  if (error) {
    return (
      <main className="mx-auto max-w-6xl px-6 py-8">
        <p className="text-rose-500">Failed to load alerts: {error.message}</p>
      </main>
    );
  }

  const rows = data ?? [];
  const groups = groupBySeverity(rows);

  return (
    <main className="mx-auto max-w-6xl px-6 py-6">
      <nav className="mb-4 text-sm">
        <Link to="/" className="text-muted-foreground hover:underline">
          ← All projects
        </Link>
      </nav>
      <header className="mb-6 flex items-baseline justify-between gap-4">
        <h1 className="text-xl font-semibold">Alerts</h1>
        <div className="flex items-center gap-3">
          <ExportMenu
            label="alerts"
            pathPrefix="/export/alerts"
            filenameStem="crosslink-alerts"
          />
          <span className="text-xs text-muted-foreground tabular-nums">
            {rows.length} open
          </span>
        </div>
      </header>

      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">No open alerts — all clear.</p>
      ) : (
        SEVERITY_ORDER.map((sev) =>
          groups[sev].length > 0 ? (
            <section key={sev} className="mb-6">
              <h2 className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                {sev} ({groups[sev].length})
              </h2>
              <ul className="space-y-2">
                {groups[sev].map((alert) => (
                  <AlertCard key={alert.id} alert={alert} />
                ))}
              </ul>
            </section>
          ) : null,
        )
      )}
    </main>
  );
}

/// Parsed `subject_ref` — `{ kind: "lock", id: "42" }` etc.
export interface SubjectRef {
  kind: string;
  id: string;
}

/// Parse a `<kind>:<id>` style `subject_ref`. Colons inside the id
/// (e.g. `commit:abc:def`) are preserved on the id side. Returns null
/// if the input isn't colon-delimited.
export function parseSubjectRef(raw: string | null): SubjectRef | null {
  if (!raw) return null;
  const idx = raw.indexOf(":");
  if (idx <= 0 || idx === raw.length - 1) return null;
  return { kind: raw.slice(0, idx), id: raw.slice(idx + 1) };
}

function AlertCard({ alert }: { alert: AlertItem }) {
  const [expanded, setExpanded] = useState(false);
  const severity = alert.severity;
  const subject = parseSubjectRef(alert.subject_ref);
  // Look up the parent project's write-capability from the shared
  // projects cache. Defaults to `"ready"` when the cache hasn't
  // populated yet (user opened /alerts as their first page) — the
  // 5s poll will hydrate it shortly after.
  const { data: projects } = useProjects();
  const projectCap: WriteCapability =
    projects?.find((p) => p.slug === alert.project_slug)?.write_capability ??
    "ready";

  // For lock-subject alerts, we fetch the project detail on expand
  // so we can show "held by <agent>" and let the user pick Release
  // vs Steal with eyes open. React Query caches this — no duplicate
  // fetch if another card on the same slug already opened it.
  const needsLockHolder =
    expanded && subject?.kind === "lock";
  const { data: projectDetail } = useProject(
    needsLockHolder ? alert.project_slug : null,
  );
  const lockEntry: LockEntry | undefined =
    subject?.kind === "lock" && subject.id
      ? projectDetail?.locks.find(
          (l) => String(l.issue_id) === subject.id,
        )
      : undefined;

  const toggle = useCallback(() => setExpanded((v) => !v), []);
  const onKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle();
      }
    },
    [toggle],
  );

  return (
    <li className={`rounded border ${SEVERITY_CLASSES[severity]}`}>
      <div
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        aria-label={`Toggle ${alert.kind} on ${alert.project_slug}`}
        onClick={toggle}
        onKeyDown={onKeyDown}
        className="flex cursor-pointer items-baseline justify-between gap-3 p-3 hover:bg-black/5"
      >
        <div className="flex flex-wrap items-baseline gap-2">
          <span
            className={`inline-block rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${SEVERITY_BADGE[severity]}`}
          >
            {alert.kind.replace(/_/g, " ")}
          </span>
          {/* stopPropagation so the link doesn't also toggle */}
          <Link
            to={`/project/${alert.project_slug}`}
            onClick={(e) => e.stopPropagation()}
            className="text-sm font-medium hover:underline"
          >
            {alert.project_slug}
          </Link>
          {alert.subject_ref && (
            <span className="text-xs text-muted-foreground">
              {alert.subject_ref}
            </span>
          )}
        </div>
        <div className="flex items-baseline gap-2">
          <span className="text-xs text-muted-foreground tabular-nums">
            {alert.opened_at}
          </span>
          <span
            aria-hidden
            className="inline-block w-3 text-center text-xs text-muted-foreground"
          >
            {expanded ? "▾" : "▸"}
          </span>
        </div>
      </div>
      {alert.detail && !expanded && (
        <p className="px-3 pb-3 text-sm text-muted-foreground">{alert.detail}</p>
      )}
      {expanded && (
        <AlertExpanded
          alert={alert}
          subject={subject}
          projectCap={projectCap}
          lockEntry={lockEntry}
        />
      )}
    </li>
  );
}

function AlertExpanded({
  alert,
  subject,
  projectCap,
  lockEntry,
}: {
  alert: AlertItem;
  subject: SubjectRef | null;
  projectCap: WriteCapability;
  lockEntry: LockEntry | undefined;
}) {
  return (
    <div
      className="border-t border-black/10 bg-black/5 px-3 py-3 text-sm"
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => e.stopPropagation()}
      role="group"
      aria-label={`${alert.kind} detail`}
    >
      {alert.detail && (
        <p className="mb-3 text-muted-foreground">{alert.detail}</p>
      )}
      <dl className="mb-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
        <dt className="text-muted-foreground">project</dt>
        <dd>
          <Link
            to={`/project/${alert.project_slug}`}
            className="font-mono hover:underline"
          >
            {alert.project_slug}
          </Link>
        </dd>
        <dt className="text-muted-foreground">kind</dt>
        <dd className="font-mono">{alert.kind}</dd>
        <dt className="text-muted-foreground">severity</dt>
        <dd className="font-mono">{alert.severity}</dd>
        {alert.subject_ref && (
          <>
            <dt className="text-muted-foreground">subject</dt>
            <dd className="font-mono">{alert.subject_ref}</dd>
          </>
        )}
        {lockEntry && (
          <>
            <dt className="text-muted-foreground">held by</dt>
            <dd className="font-mono">{lockEntry.agent_id}</dd>
            <dt className="text-muted-foreground">claimed</dt>
            <dd className="font-mono">{lockEntry.claimed_at}</dd>
          </>
        )}
        <dt className="text-muted-foreground">opened</dt>
        <dd className="font-mono">{alert.opened_at}</dd>
      </dl>
      {projectCap === "ready" ? (
        <AlertActions
          slug={alert.project_slug}
          kind={alert.kind}
          subject={subject}
          lockEntry={lockEntry}
        />
      ) : (
        <NotReadyNotice slug={alert.project_slug} capability={projectCap} />
      )}
    </div>
  );
}

/// Shown in place of `AlertActions` when the project's workspace
/// isn't ready for writes. Explains why the action buttons are
/// hidden and links to the project-detail page where the operator
/// can kick off the Initialize flow.
function NotReadyNotice({
  slug,
  capability,
}: {
  slug: string;
  capability: "not_initialized" | "agent_missing";
}) {
  const reason =
    capability === "not_initialized"
      ? "crosslink init hasn't been run"
      : "crosslink agent init hasn't been run";
  return (
    <div className="flex flex-col gap-2">
      <p className="text-xs text-amber-500">
        ⚠ Actions hidden: {reason} in this workspace. Write actions
        will fail until the workspace is initialized.
      </p>
      <div className="flex items-center gap-2">
        <Link
          to={`/project/${slug}`}
          className="rounded border border-amber-500/70 bg-amber-500/20 px-2 py-1 text-xs font-medium text-amber-600 hover:bg-amber-500/30"
        >
          Open project to initialize →
        </Link>
      </div>
    </div>
  );
}

function AlertActions({
  slug,
  kind,
  subject,
  lockEntry,
}: {
  slug: string;
  kind: string;
  subject: SubjectRef | null;
  lockEntry: LockEntry | undefined;
}) {
  // Each hook is a React rule-of-hooks mandate — call them all here,
  // decide which buttons to render based on the parsed subject.
  const closeIssue = useCloseIssue(slug);
  const commentIssue = useCommentIssue(slug);
  const releaseLock = useReleaseLock(slug);
  const stealLock = useStealLock(slug);
  const agentRequest = useAgentRequest(slug);
  const signBackfill = useSignBackfill(slug);

  const [commentOpen, setCommentOpen] = useState(false);
  const [commentText, setCommentText] = useState("");
  const anyError =
    closeIssue.error?.message ||
    commentIssue.error?.message ||
    releaseLock.error?.message ||
    stealLock.error?.message ||
    agentRequest.error?.message ||
    signBackfill.error?.message;

  // Confirmation banner: the alert row stays visible until the next
  // poll tick re-reads the hub (≤5s); without this the user can't
  // tell their click registered. Shown for any mutation's isSuccess
  // state; cleared on the next poll refresh when the alert row
  // unmounts.
  const successMessage =
    (closeIssue.isSuccess && "Issue closed — alert clears on the next poll tick.") ||
    (releaseLock.isSuccess && "Lock released — alert clears on the next poll tick.") ||
    (stealLock.isSuccess && "Lock stolen — alert clears on the next poll tick.") ||
    (commentIssue.isSuccess && "Comment posted.") ||
    (agentRequest.isSuccess && "Agent request sent — alert clears on the next poll tick.") ||
    (signBackfill.isSuccess && "Sign-backfill complete — alert clears on the next poll tick.") ||
    null;
  // Note: `kind` is consumed implicitly via the useProjectMutations
  // invalidation chain; kept as a prop so future per-kind affordances
  // (e.g. "dismiss without closing" for orphan_subissue) can branch
  // on it.
  void kind;

  // Subject-less alert → show a project-scoped shortcut only.
  if (!subject) {
    return (
      <div className="flex items-center gap-2">
        <Link
          to={`/project/${slug}`}
          className="rounded border px-2 py-1 text-xs hover:bg-black/10"
        >
          Open project →
        </Link>
      </div>
    );
  }

  const issueId = subject.kind === "lock" || subject.kind === "issue"
    ? toIssueId(subject.id)
    : null;

  return (
    <div className="flex flex-col gap-2">
      {/* Lock-subject hint: `release` requires the caller to be the
          holder; `steal` always works. We surface the holder (from
          the project-detail cache) and a short semantics reminder
          so the operator doesn't learn this via a 500-stderr dump. */}
      {subject.kind === "lock" && (
        <p className="text-xs text-muted-foreground">
          {lockEntry ? (
            <>
              Held by <code className="font-mono">{lockEntry.agent_id}</code>.
              {" "}
            </>
          ) : (
            <>(loading lock holder…) </>
          )}
          Use <strong>Release</strong> only if you're the holder. Use{" "}
          <strong>Steal</strong> to take over a stale lock held by another
          agent.
        </p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {/* Lock subject → release / steal */}
        {subject.kind === "lock" && issueId != null && (
          <>
            <button
              type="button"
              onClick={() => releaseLock.mutate(issueId)}
              disabled={releaseLock.isPending}
              className="rounded border px-2 py-1 text-xs hover:bg-black/10 disabled:opacity-50"
            >
              {releaseLock.isPending ? "Releasing…" : "Release lock"}
            </button>
            <button
              type="button"
              onClick={() => stealLock.mutate(issueId)}
              disabled={stealLock.isPending}
              className="rounded border px-2 py-1 text-xs hover:bg-black/10 disabled:opacity-50"
            >
              {stealLock.isPending ? "Stealing…" : "Steal lock"}
            </button>
          </>
        )}

        {/* Signature-invalid → sign-backfill */}
        {kind === "signature_invalid" && (
          <button
            type="button"
            onClick={() => signBackfill.mutate()}
            disabled={signBackfill.isPending}
            className="rounded border px-2 py-1 text-xs hover:bg-black/10 disabled:opacity-50"
            title="Runs `crosslink integrity sign-backfill --confirm` in the workspace"
          >
            {signBackfill.isPending ? "Backfilling…" : "Run sign-backfill"}
          </button>
        )}

        {/* Issue subject → close + comment */}
        {subject.kind === "issue" && issueId != null && (
          <>
            <button
              type="button"
              onClick={() => closeIssue.mutate(issueId)}
              disabled={closeIssue.isPending}
              className="rounded border px-2 py-1 text-xs hover:bg-black/10 disabled:opacity-50"
            >
              {closeIssue.isPending ? "Closing…" : "Close issue"}
            </button>
            <button
              type="button"
              onClick={() => setCommentOpen((v) => !v)}
              className="rounded border px-2 py-1 text-xs hover:bg-black/10"
            >
              {commentOpen ? "Cancel comment" : "Comment"}
            </button>
          </>
        )}

        {/* Agent subject → pause / resume / kill */}
        {subject.kind === "agent" && (
          <>
            <button
              type="button"
              onClick={() =>
                agentRequest.mutate({ agentId: subject.id, kind: "pause" })
              }
              disabled={agentRequest.isPending}
              className="rounded border px-2 py-1 text-xs hover:bg-black/10 disabled:opacity-50"
            >
              Pause agent
            </button>
            <button
              type="button"
              onClick={() =>
                agentRequest.mutate({ agentId: subject.id, kind: "resume" })
              }
              disabled={agentRequest.isPending}
              className="rounded border px-2 py-1 text-xs hover:bg-black/10 disabled:opacity-50"
            >
              Resume agent
            </button>
            <button
              type="button"
              onClick={() =>
                agentRequest.mutate({ agentId: subject.id, kind: "kill" })
              }
              disabled={agentRequest.isPending}
              className="rounded border px-2 py-1 text-xs text-rose-600 hover:bg-rose-500/10 disabled:opacity-50"
            >
              Kill agent
            </button>
          </>
        )}

        <Link
          to={`/project/${slug}`}
          className="ml-auto rounded border px-2 py-1 text-xs hover:bg-black/10"
        >
          Open project →
        </Link>
      </div>

      {/* Comment drawer — applies to any issue-subject alert (overdue
          or orphan), since commenting on the subissue is a reasonable
          follow-up in either case. */}
      {subject.kind === "issue" && commentOpen && issueId != null && (
        <form
          className="mt-1 flex flex-col gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (!commentText.trim()) return;
            commentIssue.mutate(
              { issueId, content: commentText },
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
            rows={3}
            placeholder="Comment text"
            className="w-full rounded border bg-background p-2 text-xs"
          />
          <div>
            <button
              type="submit"
              disabled={!commentText.trim() || commentIssue.isPending}
              className="rounded border px-2 py-1 text-xs hover:bg-black/10 disabled:opacity-50"
            >
              {commentIssue.isPending ? "Posting…" : "Post comment"}
            </button>
          </div>
        </form>
      )}

      {successMessage && (
        <p className="text-xs text-emerald-500" role="status" aria-live="polite">
          {successMessage}
        </p>
      )}
      {anyError && (
        <p className="text-xs text-rose-500">{anyError}</p>
      )}
    </div>
  );
}

/// Turn the `id` piece of a subject_ref into a numeric display_id, if
/// it looks like one. For `lock:42` this returns 42; for
/// `issue:bd1f…` (local-only UUID) it returns null — the CLI write
/// surface currently only accepts numeric IDs, so we hide the action
/// buttons until the issue has been pushed and gained a display_id.
function toIssueId(id: string): number | null {
  const trimmed = id.startsWith("#") ? id.slice(1) : id;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) && Number.isInteger(parsed) && parsed > 0
    ? parsed
    : null;
}
