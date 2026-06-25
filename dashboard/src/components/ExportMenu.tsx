// Export menu: two small pill-buttons that trigger authenticated
// downloads of the given dataset in CSV or JSON. Used on the
// Projects grid and Alerts page (Phase 5 polish — design doc §14).

import { useState } from "react";

import { downloadAuthenticated } from "@/lib/download";

interface ExportMenuProps {
  /// Display label for screen readers / tooltips, e.g. "projects".
  label: string;
  /// The server path prefix (without extension), e.g. `/export/projects`.
  /// We append `.csv` / `.json` to it.
  pathPrefix: string;
  /// The filename stem the browser should suggest, e.g. `crosslink-projects`.
  /// The extension is appended automatically.
  filenameStem: string;
}

export function ExportMenu({ label, pathPrefix, filenameStem }: ExportMenuProps) {
  const [busy, setBusy] = useState<"csv" | "json" | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function run(format: "csv" | "json") {
    setBusy(format);
    setError(null);
    try {
      await downloadAuthenticated({
        path: `${pathPrefix}.${format}`,
        filename: `${filenameStem}.${format}`,
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <span className="flex items-center gap-1 text-xs text-muted-foreground">
      <span className="hidden sm:inline">export {label}:</span>
      <button
        type="button"
        onClick={() => run("csv")}
        disabled={busy !== null}
        aria-label={`Download ${label} as CSV`}
        className="rounded border px-2 py-0.5 hover:bg-accent/10 disabled:opacity-50"
      >
        {busy === "csv" ? "…" : "CSV"}
      </button>
      <button
        type="button"
        onClick={() => run("json")}
        disabled={busy !== null}
        aria-label={`Download ${label} as JSON`}
        className="rounded border px-2 py-0.5 hover:bg-accent/10 disabled:opacity-50"
      >
        {busy === "json" ? "…" : "JSON"}
      </button>
      {error && <span className="text-rose-400">{error}</span>}
    </span>
  );
}
