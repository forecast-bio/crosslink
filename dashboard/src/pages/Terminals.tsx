// /terminals — list of live + recently-exited PTY sessions, with a
// spawn form and an attach pane backed by xterm.js. Replaces the
// previous "drop to a real terminal" friction for design / kickoff
// sessions launched from the dashboard.

import { useState } from "react";

import { useProjects, useSpawnPty, usePtySessions } from "@/api/client";
import { PtyTerminal } from "@/components/PtyTerminal";

export function Terminals() {
  const sessions = usePtySessions();
  const projects = useProjects();
  const spawn = useSpawnPty();

  const [activeId, setActiveId] = useState<string | null>(null);
  const [projectSlug, setProjectSlug] = useState<string>("");
  const [command, setCommand] = useState<string>("crosslink design");
  const [argsRaw, setArgsRaw] = useState<string>("");

  const tracked = projects.data ?? [];

  return (
    <main className="mx-auto max-w-6xl px-6 py-6">
      <header className="mb-4 flex items-baseline justify-between">
        <h1 className="text-2xl font-semibold">Terminals</h1>
        <p className="text-xs text-muted-foreground">
          Each terminal is a real PTY on the dashboard host. Closing the
          tab does not kill the process — reattach from this page.
        </p>
      </header>

      <section className="mb-6 rounded border bg-card p-3">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Spawn new terminal
        </h2>
        <form
          className="flex flex-col gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (!projectSlug || !command.trim()) return;
            const parsed = argsRaw
              .split(" ")
              .map((s) => s.trim())
              .filter(Boolean);
            spawn.mutate(
              {
                project_slug: projectSlug,
                command: command.trim(),
                args: parsed,
              },
              {
                onSuccess: (s) => {
                  setActiveId(s.id);
                  setArgsRaw("");
                },
              },
            );
          }}
        >
          <div className="flex flex-wrap items-center gap-2">
            <label className="text-xs text-muted-foreground w-20">Project</label>
            <select
              value={projectSlug}
              onChange={(e) => setProjectSlug(e.target.value)}
              className="rounded border bg-background px-2 py-1 text-sm"
            >
              <option value="">— select —</option>
              {tracked.map((p) => (
                <option key={p.slug} value={p.slug}>
                  {p.slug}
                </option>
              ))}
            </select>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <label className="text-xs text-muted-foreground w-20">Command</label>
            <input
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="e.g. crosslink design"
              className="flex-1 rounded border bg-background px-2 py-1 text-sm"
            />
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <label className="text-xs text-muted-foreground w-20">Args</label>
            <input
              value={argsRaw}
              onChange={(e) => setArgsRaw(e.target.value)}
              placeholder='space-separated, e.g. "run --plan plan.md"'
              className="flex-1 rounded border bg-background px-2 py-1 text-sm"
            />
          </div>
          <div className="flex items-center gap-2">
            <button
              type="submit"
              disabled={!projectSlug || !command.trim() || spawn.isPending}
              className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10 disabled:opacity-50"
            >
              {spawn.isPending ? "Spawning…" : "Spawn"}
            </button>
            <SpawnShortcuts onPick={(c) => setCommand(c)} />
            {spawn.error && (
              <span className="text-xs text-rose-500">{spawn.error.message}</span>
            )}
          </div>
        </form>
      </section>

      <section className="mb-6">
        <h2 className="mb-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Sessions ({sessions.data?.length ?? 0})
        </h2>
        {!sessions.data || sessions.data.length === 0 ? (
          <p className="text-sm text-muted-foreground">No terminals yet.</p>
        ) : (
          <ul className="divide-y divide-border rounded border bg-card">
            {sessions.data.map((s) => (
              <li
                key={s.id}
                className={`flex flex-wrap items-baseline justify-between gap-2 px-3 py-2 text-sm ${
                  activeId === s.id ? "bg-accent/10" : ""
                }`}
              >
                <span className="flex items-baseline gap-2">
                  <span className="font-mono text-xs text-muted-foreground">
                    {s.id}
                  </span>
                  <span className="font-medium">{s.command}</span>
                  <span className="text-xs text-muted-foreground">
                    in {s.project_slug}
                  </span>
                </span>
                <span className="flex items-center gap-2">
                  <span className="text-xs text-muted-foreground tabular-nums">
                    {s.started_at.replace("T", " ").slice(0, 19)}
                  </span>
                  {s.exit_code != null && (
                    <span className="rounded-full bg-amber-500/20 px-2 py-0.5 text-[10px] text-amber-500">
                      exited {s.exit_code}
                    </span>
                  )}
                  <button
                    type="button"
                    onClick={() => setActiveId(s.id)}
                    className="rounded border px-2 py-0.5 text-xs hover:bg-accent/10"
                  >
                    Attach
                  </button>
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      {activeId && (
        <section>
          <div className="rounded border bg-black">
            <PtyTerminal sessionId={activeId} />
          </div>
        </section>
      )}
    </main>
  );
}

function SpawnShortcuts({ onPick }: { onPick: (c: string) => void }) {
  return (
    <span className="ml-2 flex items-center gap-1 text-xs text-muted-foreground">
      shortcuts:
      {[
        { label: "design", cmd: "crosslink design" },
        { label: "kickoff", cmd: "crosslink kickoff run" },
        { label: "shell", cmd: "/bin/bash" },
      ].map((s) => (
        <button
          key={s.cmd}
          type="button"
          onClick={() => onPick(s.cmd)}
          className="rounded border px-1.5 py-0.5 hover:bg-accent/10"
        >
          {s.label}
        </button>
      ))}
    </span>
  );
}
