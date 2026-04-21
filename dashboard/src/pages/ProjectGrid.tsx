// Default route — multi-project tile grid. Calls `useProjects()` and
// renders a `ProjectTile` per row. Empty state has the same "no
// projects tracked" help text that `crosslink dashboard list` prints.

import { ExportMenu } from "@/components/ExportMenu";
import { ProjectTile } from "@/components/ProjectTile";
import { useProjects } from "@/api/client";

export function ProjectGrid() {
  const { data, isLoading, error } = useProjects();

  if (isLoading) {
    return (
      <main className="mx-auto max-w-6xl px-6 py-8">
        <p className="text-muted-foreground">Loading projects…</p>
      </main>
    );
  }

  if (error) {
    return (
      <main className="mx-auto max-w-6xl px-6 py-8">
        <p className="text-rose-500">Failed to load projects: {error.message}</p>
      </main>
    );
  }

  const projects = data ?? [];

  if (projects.length === 0) {
    return (
      <main className="mx-auto max-w-3xl px-6 py-16">
        <h1 className="text-2xl font-semibold">crosslink dashboard</h1>
        <p className="mt-4 text-muted-foreground">No tracked projects yet.</p>
        <p className="mt-2 text-sm text-muted-foreground">
          Add one from a terminal:{" "}
          <code className="rounded bg-muted px-1 py-0.5 text-xs">
            crosslink dashboard track &lt;owner/repo&gt;
          </code>
        </p>
      </main>
    );
  }

  return (
    <main className="mx-auto max-w-6xl px-6 py-6">
      <header className="mb-4 flex items-baseline justify-between gap-4">
        <h1 className="text-xl font-semibold">Projects</h1>
        <div className="flex items-center gap-3">
          <ExportMenu
            label="projects"
            pathPrefix="/export/projects"
            filenameStem="crosslink-projects"
          />
          <span className="text-xs text-muted-foreground tabular-nums">
            {projects.length} tracked
          </span>
        </div>
      </header>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
        {projects.map((p) => (
          <ProjectTile key={p.slug} item={p} />
        ))}
      </div>
    </main>
  );
}
