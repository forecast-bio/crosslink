// /settings/github — GitHub integration panel. Lets the operator
// store a PAT (Phase 4.1 backend does the AES-GCM encryption), set a
// default org, enumerate crosslink-touched repos, and auto-track
// every hit in one click.
//
// The backend endpoints live in crosslink/src/dashboard/github_api.rs.

import { useState, type FormEvent } from "react";

import {
  useCloneRepo,
  useGithubConfig,
  useOrgRepos,
  useSetGithubConfig,
  useTrackAllOrg,
} from "@/api/client";
import type { CloneRepoOutcome, GithubTrackAllOutcome } from "@/api/types";

export function SettingsGithub() {
  const config = useGithubConfig();
  const setConfig = useSetGithubConfig();
  const trackAll = useTrackAllOrg();
  const cloneRepo = useCloneRepo();

  const [tokenInput, setTokenInput] = useState("");
  const [orgInput, setOrgInput] = useState("");
  const [browseOrg, setBrowseOrg] = useState<string | null>(null);
  const [browseRequested, setBrowseRequested] = useState(false);
  const [cloneRoot, setCloneRoot] = useState("");
  const [initOnTrack, setInitOnTrack] = useState(false);
  const [agentIdInput, setAgentIdInput] = useState("");
  const [trackOutcome, setTrackOutcome] =
    useState<GithubTrackAllOutcome | null>(null);

  // Standalone clone-by-URL state (independent of PAT / org).
  const [cloneUrl, setCloneUrl] = useState("");
  const [cloneSlug, setCloneSlug] = useState("");
  const [cloneInitToggle, setCloneInitToggle] = useState(false);
  const [cloneAgentId, setCloneAgentId] = useState("");
  const [cloneOutcome, setCloneOutcome] = useState<CloneRepoOutcome | null>(
    null,
  );

  const repos = useOrgRepos(browseOrg, browseRequested);

  const currentOrg = config.data?.default_org ?? "";

  function submitToken(e: FormEvent) {
    e.preventDefault();
    if (!tokenInput.trim()) return;
    setConfig.mutate(
      { token: tokenInput.trim() },
      {
        onSuccess: () => setTokenInput(""),
      },
    );
  }

  function submitOrg(e: FormEvent) {
    e.preventDefault();
    const trimmed = orgInput.trim();
    setConfig.mutate({ default_org: trimmed === "" ? null : trimmed });
  }

  function deleteToken() {
    if (!window.confirm("Remove stored GitHub PAT?")) return;
    setConfig.mutate({ token: "" });
  }

  function browse(org: string) {
    const trimmed = org.trim();
    if (!trimmed) return;
    setBrowseOrg(trimmed);
    setBrowseRequested(true);
  }

  function onTrackAll() {
    if (!browseOrg) return;
    trackAll.mutate(
      {
        org: browseOrg,
        cloneRoot: cloneRoot.trim() || undefined,
        init: initOnTrack,
        agentId: initOnTrack ? agentIdInput.trim() : undefined,
      },
      {
        onSuccess: (data) => setTrackOutcome(data),
      },
    );
  }

  const trackAllDisabled =
    trackAll.isPending ||
    (initOnTrack && agentIdInput.trim() === "");

  function submitCloneByUrl(e: FormEvent) {
    e.preventDefault();
    const url = cloneUrl.trim();
    if (!url) return;
    if (cloneInitToggle && cloneAgentId.trim() === "") return;
    cloneRepo.mutate(
      {
        url,
        slug: cloneSlug.trim() || undefined,
        init: cloneInitToggle,
        agentId: cloneInitToggle ? cloneAgentId.trim() : undefined,
      },
      {
        onSuccess: (outcome) => {
          setCloneOutcome(outcome);
          setCloneUrl("");
          setCloneSlug("");
        },
      },
    );
  }

  const cloneDisabled =
    cloneRepo.isPending ||
    cloneUrl.trim() === "" ||
    (cloneInitToggle && cloneAgentId.trim() === "");

  return (
    <main className="mx-auto max-w-4xl px-6 py-6">
      <header className="mb-4">
        <h1 className="text-2xl font-semibold">GitHub integration</h1>
        <p className="text-xs text-muted-foreground">
          Store a personal access token, pick a default org, and
          auto-track every repo in the org that already has a{" "}
          <code className="rounded bg-muted px-1">crosslink/hub</code>{" "}
          branch.
        </p>
      </header>

      {config.error && (
        <p className="mb-4 rounded border border-rose-500/50 bg-rose-500/10 px-3 py-2 text-sm text-rose-300">
          Failed to load config: {config.error.message}
        </p>
      )}

      <section className="mb-6 rounded border bg-card p-4">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Personal access token
        </h2>
        <div className="mb-3 text-sm">
          {config.isLoading ? (
            <span className="text-muted-foreground">Loading…</span>
          ) : config.data?.token_present ? (
            <span className="flex flex-wrap items-center gap-2">
              <span className="rounded-full bg-emerald-500/20 px-2 py-0.5 text-xs text-emerald-400">
                configured
              </span>
              <code className="font-mono text-xs">
                {config.data.token_fingerprint ?? "—"}
              </code>
              <button
                type="button"
                onClick={deleteToken}
                disabled={setConfig.isPending}
                className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
              >
                Remove
              </button>
            </span>
          ) : (
            <span className="text-muted-foreground">No token stored.</span>
          )}
        </div>
        <form onSubmit={submitToken} className="flex flex-wrap items-center gap-2">
          <input
            type="password"
            autoComplete="off"
            value={tokenInput}
            onChange={(e) => setTokenInput(e.target.value)}
            placeholder="ghp_… or github_pat_…"
            className="flex-1 min-w-[20rem] rounded border bg-background px-2 py-1 font-mono text-sm"
          />
          <button
            type="submit"
            disabled={!tokenInput.trim() || setConfig.isPending}
            className="rounded border px-2 py-1 text-xs hover:bg-accent/10 disabled:opacity-50"
          >
            {setConfig.isPending ? "Saving…" : "Save token"}
          </button>
        </form>
        <p className="mt-2 text-xs text-muted-foreground">
          The token is stored AES-256-GCM-encrypted in the dashboard DB,
          keyed to this machine. See design doc §14 — this is obfuscation
          against casual read, not defense against an attacker with full
          disk access.
        </p>
      </section>

      <section className="mb-6 rounded border bg-card p-4">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Default organization
        </h2>
        <form onSubmit={submitOrg} className="flex flex-wrap items-center gap-2">
          <input
            value={orgInput}
            onChange={(e) => setOrgInput(e.target.value)}
            placeholder={currentOrg || "e.g. my-org"}
            className="flex-1 min-w-[16rem] rounded border bg-background px-2 py-1 text-sm"
          />
          <button
            type="submit"
            disabled={setConfig.isPending}
            className="rounded border px-2 py-1 text-xs hover:bg-accent/10 disabled:opacity-50"
          >
            {setConfig.isPending ? "Saving…" : "Save org"}
          </button>
          {currentOrg && (
            <span className="text-xs text-muted-foreground">
              current: <code className="font-mono">{currentOrg}</code>
            </span>
          )}
        </form>
      </section>

      <section className="mb-6 rounded border bg-card p-4">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Clone &amp; track a single repo
        </h2>
        <p className="mb-3 text-xs text-muted-foreground">
          Paste any git URL (<code className="font-mono">https://…</code> or{" "}
          <code className="font-mono">git@…</code>) to clone it into{" "}
          <code className="font-mono">~/&lt;repo&gt;</code>{" "}
          (flat, next to your manual clones) and start polling it.
          Independent of the PAT — useful for adding a single repo
          or one that's not in a browsable org.
        </p>
        <form onSubmit={submitCloneByUrl} className="flex flex-col gap-2">
          <input
            value={cloneUrl}
            onChange={(e) => setCloneUrl(e.target.value)}
            placeholder="git url (https://github.com/owner/repo.git or git@…)"
            className="w-full rounded border bg-background px-2 py-1 font-mono text-sm"
            aria-label="Clone URL"
          />
          <input
            value={cloneSlug}
            onChange={(e) => setCloneSlug(e.target.value)}
            placeholder="slug override (optional, defaults to owner/repo from URL)"
            className="w-full rounded border bg-background px-2 py-1 font-mono text-sm"
            aria-label="Slug override"
          />
          <label className="flex cursor-pointer items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={cloneInitToggle}
              onChange={(e) => setCloneInitToggle(e.target.checked)}
            />
            <span>
              Initialize after clone (runs{" "}
              <code className="font-mono">crosslink init</code> +{" "}
              <code className="font-mono">crosslink agent init</code>)
            </span>
          </label>
          {cloneInitToggle && (
            <input
              value={cloneAgentId}
              onChange={(e) => setCloneAgentId(e.target.value)}
              placeholder="agent id (alphanumeric, hyphens, underscores)"
              className="w-full rounded border bg-background px-2 py-1 font-mono text-sm"
              aria-label="Clone agent ID"
            />
          )}
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="submit"
              disabled={cloneDisabled}
              className="rounded border px-2 py-1 text-xs hover:bg-accent/10 disabled:opacity-50"
            >
              {cloneRepo.isPending ? "Cloning…" : "Clone & track"}
            </button>
            {cloneInitToggle && cloneAgentId.trim() === "" && (
              <span className="text-xs text-amber-500">
                Agent id required when “Initialize” is on.
              </span>
            )}
            {cloneRepo.error && (
              <span className="text-xs text-rose-400">
                {cloneRepo.error.message}
              </span>
            )}
          </div>
        </form>
        {cloneOutcome && (
          <p className="mt-3 text-xs text-emerald-500" role="status">
            ✓ Cloned + tracked <code className="font-mono">{cloneOutcome.slug}</code>{" "}
            at <code className="font-mono">{cloneOutcome.clone_path}</code>
            {cloneOutcome.initialized ? " (initialized)" : ""}.
          </p>
        )}
      </section>

      <section className="mb-6 rounded border bg-card p-4">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Discover &amp; track repos
        </h2>
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={() => browse(currentOrg)}
            disabled={!currentOrg || !config.data?.token_present}
            className="rounded border px-2 py-1 text-xs hover:bg-accent/10 disabled:opacity-50"
            title={
              !config.data?.token_present
                ? "Save a token first"
                : !currentOrg
                  ? "Save a default org first"
                  : ""
            }
          >
            Browse {currentOrg || "default org"}
          </button>
          {browseOrg && (
            <span className="text-xs text-muted-foreground">
              browsing <code className="font-mono">{browseOrg}</code>
            </span>
          )}
        </div>

        {repos.isFetching && (
          <p className="text-sm text-muted-foreground">Enumerating repos…</p>
        )}
        {repos.error && (
          <p className="text-sm text-rose-400">
            GitHub API error: {repos.error.message}
          </p>
        )}
        {repos.data && repos.data.length === 0 && (
          <p className="text-sm text-muted-foreground">
            No repos in this org have a{" "}
            <code className="font-mono">crosslink/hub</code> branch yet.
          </p>
        )}
        {repos.data && repos.data.length > 0 && (
          <>
            <ul className="mb-3 divide-y divide-border rounded border bg-background">
              {repos.data.map((r) => (
                <li
                  key={r.full_name}
                  className="flex items-baseline justify-between px-3 py-2 text-sm"
                >
                  <span className="font-mono">{r.full_name}</span>
                  <span className="text-xs text-muted-foreground">
                    default: {r.default_branch}
                  </span>
                </li>
              ))}
            </ul>
            <div className="flex flex-col gap-2">
              <input
                value={cloneRoot}
                onChange={(e) => setCloneRoot(e.target.value)}
                placeholder="clone root (defaults to $HOME — repos land at ~/<repo>)"
                className="w-full rounded border bg-background px-2 py-1 text-sm"
              />
              <label className="flex cursor-pointer items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={initOnTrack}
                  onChange={(e) => setInitOnTrack(e.target.checked)}
                />
                <span>
                  Initialize cloned repos (run{" "}
                  <code className="font-mono">crosslink init</code> +{" "}
                  <code className="font-mono">crosslink agent init</code> in each
                  clone so dashboard write actions work)
                </span>
              </label>
              {initOnTrack && (
                <input
                  value={agentIdInput}
                  onChange={(e) => setAgentIdInput(e.target.value)}
                  placeholder="agent id (alphanumeric, hyphens, underscores)"
                  className="w-full rounded border bg-background px-2 py-1 font-mono text-sm"
                  aria-label="Agent ID"
                />
              )}
              <div className="flex flex-wrap items-center gap-2">
                <button
                  type="button"
                  onClick={onTrackAll}
                  disabled={trackAllDisabled}
                  className="rounded border px-2 py-1 text-xs hover:bg-accent/10 disabled:opacity-50"
                >
                  {trackAll.isPending
                    ? "Tracking…"
                    : `Track all ${repos.data.length}`}
                </button>
                {initOnTrack && agentIdInput.trim() === "" && (
                  <span className="text-xs text-amber-500">
                    Agent id required when “Initialize” is on.
                  </span>
                )}
                {trackAll.error && (
                  <span className="text-xs text-rose-400">
                    {trackAll.error.message}
                  </span>
                )}
              </div>
            </div>
          </>
        )}

        {trackOutcome && (
          <div className="mt-4 rounded border border-border/60 bg-background p-3 text-sm">
            <p className="mb-1 font-semibold">
              Tracked {trackOutcome.tracked.length}, skipped{" "}
              {trackOutcome.skipped.length}
            </p>
            {trackOutcome.tracked.length > 0 && (
              <details className="mb-1">
                <summary className="cursor-pointer text-xs text-emerald-400">
                  Tracked ({trackOutcome.tracked.length})
                </summary>
                <ul className="mt-1 list-disc pl-5 text-xs text-muted-foreground">
                  {trackOutcome.tracked.map((slug) => (
                    <li key={slug} className="font-mono">
                      {slug}
                    </li>
                  ))}
                </ul>
              </details>
            )}
            {trackOutcome.skipped.length > 0 && (
              <details>
                <summary className="cursor-pointer text-xs text-amber-400">
                  Skipped ({trackOutcome.skipped.length})
                </summary>
                <ul className="mt-1 space-y-1 text-xs text-muted-foreground">
                  {trackOutcome.skipped.map((s) => (
                    <li key={s.slug}>
                      <span className="font-mono">{s.slug}</span> — {s.reason}
                    </li>
                  ))}
                </ul>
              </details>
            )}
          </div>
        )}
      </section>
    </main>
  );
}
