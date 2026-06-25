// Audible-alert player for dashboard alert fires (design doc §14
// Phase 5 — polish). Synthesises a short tone with the WebAudio API
// rather than shipping an MP3 asset:
//   - zero asset pipeline, zero bundle growth
//   - each severity gets a distinct pitch so operators can
//     recognise the event by ear without glancing at the screen
//
// WebAudio contexts must be created lazily after a user gesture in
// most browsers. We initialise on the first call to `playToneFor` —
// if the caller hasn't interacted yet the audio context will be in
// "suspended" state and the tone silently skipped. That's acceptable:
// the *first* alert after page load may be silent, every subsequent
// one plays.

import type { AlertSeverity } from "@/api/types";

import { getPreferences, subscribePreferences } from "./preferences";
import { subscribeAlertOpens } from "@/api/ws";

/// Single AudioContext reused across tones. Lazily created.
let ctx: AudioContext | null = null;

function audioContext(): AudioContext | null {
  if (typeof window === "undefined") return null;
  if (ctx) return ctx;
  const AC =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext })
      .webkitAudioContext;
  if (!AC) return null;
  try {
    ctx = new AC();
    return ctx;
  } catch {
    return null;
  }
}

/// Base tones in Hz — a simple major-third-ish cluster that reads as
/// info → warning → critical (low → high → higher) and doesn't need
/// musical training to distinguish.
const TONE_HZ: Record<AlertSeverity, number> = {
  info: 440, // A4
  warning: 587, // D5
  critical: 784, // G5
};

/// Synthesise one short tone for the given severity. Returns true if
/// something was actually played.
export function playToneFor(severity: AlertSeverity): boolean {
  const ac = audioContext();
  if (!ac) return false;
  // "suspended" means the browser is withholding audio until a user
  // gesture. We don't try to resume() here — resume requires the
  // click to be ancestrally in the call stack, which isn't true of
  // our async WS dispatch. Future alerts (post-interaction) will play.
  if (ac.state !== "running") {
    void ac.resume().catch(() => {
      // Ignore; next gesture will unblock.
    });
    return false;
  }
  try {
    const osc = ac.createOscillator();
    const gain = ac.createGain();
    osc.type = "sine";
    osc.frequency.value = TONE_HZ[severity];
    // 220ms envelope: quick ramp up, hold, exponential tail.
    const now = ac.currentTime;
    gain.gain.setValueAtTime(0, now);
    gain.gain.linearRampToValueAtTime(0.15, now + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.001, now + 0.25);
    osc.connect(gain).connect(ac.destination);
    osc.start(now);
    osc.stop(now + 0.27);
    return true;
  } catch {
    return false;
  }
}

/// Event emitted by the WS layer on each fired alert (post-dedupe
/// against the server-side reconcile). Individual-alert severity
/// isn't in the WS payload today, so the caller passes the worst
/// severity it knows about.
export interface AlertOpenedEvent {
  slug: string;
  opened: number;
  /// Optional: the worst severity in this batch. When absent we
  /// conservatively assume "critical" so the sound still fires.
  worstSeverity?: AlertSeverity;
}

/// Install the bridge. Subscribes to WS alert-opened events, checks
/// the preferences store on each event, and plays the appropriate
/// tone. Returns a disposer.
export function installAlertSoundBridge(): () => void {
  // We read prefs on every event rather than caching, so toggling
  // audible_enabled takes effect immediately — no reconnect needed.
  const unsubWs = subscribeAlertOpens((event) => {
    const prefs = getPreferences();
    if (!prefs.audibleEnabled) return;
    if (event.opened <= 0) return;
    const severity = event.worstSeverity ?? "critical";
    if (!prefs.audibleSeverities.includes(severity)) return;
    playToneFor(severity);
  });

  // Keep the subscribePreferences reference so the linter doesn't
  // flag the import as unused; the reactive behaviour is the per-
  // event read above, but some future work might want change-driven
  // behaviour (e.g. replaying a tone when the preference flips back
  // on) — keep this wire intact and documented.
  const unsubPref = subscribePreferences(() => {
    // Currently a no-op; we read prefs at play-time instead.
  });

  return () => {
    unsubWs();
    unsubPref();
  };
}
