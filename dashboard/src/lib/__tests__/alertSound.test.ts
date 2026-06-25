// Coverage for the audible-alert bridge. We stub AudioContext so the
// test environment doesn't need a real audio device, then drive
// synthetic WS events through __emitAlertOpenForTests and assert the
// bridge called playToneFor / obeyed the preferences filter.

import { beforeEach, describe, expect, it, vi } from "vitest";

import { __emitAlertOpenForTests } from "@/api/ws";
import {
  __resetForTests,
  setPreferences,
} from "../preferences";
import { installAlertSoundBridge } from "../alertSound";

interface StubOsc {
  connect: ReturnType<typeof vi.fn>;
  start: ReturnType<typeof vi.fn>;
  stop: ReturnType<typeof vi.fn>;
  frequency: { value: number };
  type: OscillatorType;
}
interface StubGain {
  connect: ReturnType<typeof vi.fn>;
  gain: {
    setValueAtTime: ReturnType<typeof vi.fn>;
    linearRampToValueAtTime: ReturnType<typeof vi.fn>;
    exponentialRampToValueAtTime: ReturnType<typeof vi.fn>;
  };
}

function stubAudioContext(state: "running" | "suspended" = "running"): {
  createOscillator: ReturnType<typeof vi.fn>;
  createGain: ReturnType<typeof vi.fn>;
  lastOsc: StubOsc | null;
} {
  const wrapper = {
    createOscillator: vi.fn(),
    createGain: vi.fn(),
    lastOsc: null as StubOsc | null,
  };
  wrapper.createOscillator.mockImplementation(() => {
    const osc: StubOsc = {
      connect: vi.fn(() => ({ connect: vi.fn() })),
      start: vi.fn(),
      stop: vi.fn(),
      frequency: { value: 0 },
      type: "sine",
    };
    wrapper.lastOsc = osc;
    return osc;
  });
  wrapper.createGain.mockImplementation(() => {
    const g: StubGain = {
      connect: vi.fn((node) => node),
      gain: {
        setValueAtTime: vi.fn(),
        linearRampToValueAtTime: vi.fn(),
        exponentialRampToValueAtTime: vi.fn(),
      },
    };
    return g;
  });

  (globalThis as unknown as { AudioContext: unknown }).AudioContext =
    vi.fn().mockImplementation(() => ({
      state,
      currentTime: 0,
      destination: {},
      resume: vi.fn().mockResolvedValue(undefined),
      createOscillator: wrapper.createOscillator,
      createGain: wrapper.createGain,
    }));

  return wrapper;
}

describe("alertSound bridge", () => {
  beforeEach(() => {
    window.localStorage.clear();
    __resetForTests();
  });

  it("plays a tone when an alert fires and severity is enabled", async () => {
    const audio = stubAudioContext("running");
    setPreferences({
      theme: "dark",
      audibleEnabled: true,
      audibleSeverities: ["critical"],
    });
    const dispose = installAlertSoundBridge();

    __emitAlertOpenForTests({ slug: "owner/repo", opened: 1, resolved: 0 });

    expect(audio.createOscillator).toHaveBeenCalledTimes(1);
    // Default worstSeverity is "critical" — the G5 tone.
    expect(audio.lastOsc?.frequency.value).toBe(784);

    dispose();
  });

  it("stays silent when audibleEnabled is false", async () => {
    const audio = stubAudioContext("running");
    setPreferences({
      theme: "dark",
      audibleEnabled: false,
      audibleSeverities: ["critical"],
    });
    const dispose = installAlertSoundBridge();

    __emitAlertOpenForTests({ slug: "owner/repo", opened: 1, resolved: 0 });

    expect(audio.createOscillator).not.toHaveBeenCalled();
    dispose();
  });

  it("stays silent when the severity isn't in the allow-list", async () => {
    const audio = stubAudioContext("running");
    setPreferences({
      theme: "dark",
      audibleEnabled: true,
      audibleSeverities: ["warning"],
    });
    const dispose = installAlertSoundBridge();

    // Default worstSeverity fallback is "critical" — not in the list.
    __emitAlertOpenForTests({ slug: "owner/repo", opened: 1, resolved: 0 });

    expect(audio.createOscillator).not.toHaveBeenCalled();
    dispose();
  });

  it("stays silent when opened == 0 (no fires, only resolves)", async () => {
    const audio = stubAudioContext("running");
    setPreferences({
      theme: "dark",
      audibleEnabled: true,
      audibleSeverities: ["critical"],
    });
    const dispose = installAlertSoundBridge();

    __emitAlertOpenForTests({ slug: "owner/repo", opened: 0, resolved: 3 });

    expect(audio.createOscillator).not.toHaveBeenCalled();
    dispose();
  });

  it("dispose unsubscribes from the WS bus", async () => {
    const audio = stubAudioContext("running");
    setPreferences({
      theme: "dark",
      audibleEnabled: true,
      audibleSeverities: ["critical"],
    });
    const dispose = installAlertSoundBridge();
    dispose();

    __emitAlertOpenForTests({ slug: "owner/repo", opened: 1, resolved: 0 });

    expect(audio.createOscillator).not.toHaveBeenCalled();
  });

  it("skips playback when the audio context is suspended (pre-gesture)", async () => {
    const audio = stubAudioContext("suspended");
    setPreferences({
      theme: "dark",
      audibleEnabled: true,
      audibleSeverities: ["critical"],
    });
    const dispose = installAlertSoundBridge();
    __emitAlertOpenForTests({ slug: "owner/repo", opened: 1, resolved: 0 });

    expect(audio.createOscillator).not.toHaveBeenCalled();
    dispose();
  });
});
