// /settings/preferences — user-level UI preferences (design doc §14
// Phase 5 — polish). Covers theme selection + audible-alert config.
//
// Values are persisted in localStorage via `lib/preferences.ts`. The
// theme observer + alert-sound bridge are mounted near the app root
// (see App.tsx) and react to changes automatically — this page only
// writes.

import type { AlertSeverity } from "@/api/types";
import {
  patchPreferences,
  usePreferences,
  type ThemePreference,
} from "@/lib/preferences";

const THEMES: Array<{ value: ThemePreference; label: string; hint: string }> = [
  { value: "system", label: "System", hint: "follow OS colour scheme" },
  { value: "dark", label: "Dark", hint: "default SCADA palette" },
  { value: "light", label: "Light", hint: "high-contrast day mode" },
];

const SEVERITIES: Array<{ value: AlertSeverity; label: string }> = [
  { value: "critical", label: "Critical" },
  { value: "warning", label: "Warning" },
  { value: "info", label: "Info" },
];

export function SettingsPreferences() {
  const prefs = usePreferences();

  function toggleSeverity(sev: AlertSeverity) {
    const has = prefs.audibleSeverities.includes(sev);
    const next: AlertSeverity[] = has
      ? prefs.audibleSeverities.filter((s) => s !== sev)
      : [...prefs.audibleSeverities, sev];
    // Keep canonical order (critical > warning > info) so the stored
    // list stays tidy regardless of click order.
    const order: AlertSeverity[] = ["critical", "warning", "info"];
    next.sort((a, b) => order.indexOf(a) - order.indexOf(b));
    patchPreferences({ audibleSeverities: next });
  }

  return (
    <main className="mx-auto max-w-3xl px-6 py-6">
      <header className="mb-6">
        <h1 className="text-2xl font-semibold">Preferences</h1>
        <p className="text-xs text-muted-foreground">
          Personal UI settings. Stored locally in this browser — nothing
          lands in the dashboard DB or syncs between devices.
        </p>
      </header>

      <section className="mb-6 rounded border bg-card p-4">
        <h2 className="mb-3 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Theme
        </h2>
        <fieldset className="flex flex-wrap gap-2">
          <legend className="sr-only">Theme</legend>
          {THEMES.map(({ value, label, hint }) => (
            <label
              key={value}
              className={`flex cursor-pointer flex-col rounded border px-3 py-2 text-sm hover:bg-accent/10 ${
                prefs.theme === value ? "bg-accent/20 font-semibold" : ""
              }`}
            >
              <span className="flex items-center gap-2">
                <input
                  type="radio"
                  name="theme"
                  value={value}
                  checked={prefs.theme === value}
                  onChange={() => patchPreferences({ theme: value })}
                />
                {label}
              </span>
              <span className="text-xs text-muted-foreground">{hint}</span>
            </label>
          ))}
        </fieldset>
      </section>

      <section className="mb-6 rounded border bg-card p-4">
        <h2 className="mb-3 text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Audible alerts
        </h2>
        <label className="mb-3 flex cursor-pointer items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={prefs.audibleEnabled}
            onChange={(e) =>
              patchPreferences({ audibleEnabled: e.target.checked })
            }
          />
          Play a tone when an alert fires
        </label>
        <p className="mb-3 text-xs text-muted-foreground">
          Tones are synthesised in-browser — no external assets loaded.
          Some browsers block audio until you interact with the page;
          if the first alert is silent, any subsequent fire will play.
        </p>
        <fieldset
          className={`flex flex-wrap gap-2 ${
            prefs.audibleEnabled ? "" : "opacity-40"
          }`}
          disabled={!prefs.audibleEnabled}
        >
          <legend className="sr-only">Severities</legend>
          {SEVERITIES.map(({ value, label }) => {
            const checked = prefs.audibleSeverities.includes(value);
            return (
              <label
                key={value}
                className={`flex cursor-pointer items-center gap-2 rounded border px-3 py-1.5 text-xs hover:bg-accent/10 ${
                  checked ? "bg-accent/20" : ""
                }`}
              >
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={() => toggleSeverity(value)}
                />
                {label}
              </label>
            );
          })}
        </fieldset>
      </section>
    </main>
  );
}
