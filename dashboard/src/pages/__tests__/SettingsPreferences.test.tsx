// Coverage for the /settings/preferences page. Exercises the
// preferences store directly (no mocks) — the store is pure JS and
// already covered by preferences.test.ts, so here we just assert that
// the UI mirrors + mutates it correctly.

import "@testing-library/jest-dom/vitest";
import { beforeEach, describe, expect, it } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import {
  __resetForTests,
  getPreferences,
  setPreferences,
} from "@/lib/preferences";
import { SettingsPreferences } from "../SettingsPreferences";

describe("SettingsPreferences page", () => {
  beforeEach(() => {
    window.localStorage.clear();
    __resetForTests();
  });

  it("renders all three theme radios with 'System' selected by default", () => {
    render(<SettingsPreferences />);
    const sysRadio = screen.getByRole("radio", { name: /system/i });
    const darkRadio = screen.getByRole("radio", { name: /dark/i });
    const lightRadio = screen.getByRole("radio", { name: /light/i });
    expect(sysRadio).toBeChecked();
    expect(darkRadio).not.toBeChecked();
    expect(lightRadio).not.toBeChecked();
  });

  it("picking a theme updates the store", () => {
    render(<SettingsPreferences />);
    fireEvent.click(screen.getByRole("radio", { name: /light/i }));
    expect(getPreferences().theme).toBe("light");
  });

  it("audible toggle is off by default and enabling it persists", () => {
    render(<SettingsPreferences />);
    const toggle = screen.getByRole("checkbox", {
      name: /play a tone when an alert fires/i,
    });
    expect(toggle).not.toBeChecked();
    fireEvent.click(toggle);
    expect(getPreferences().audibleEnabled).toBe(true);
  });

  it("severity checkboxes default to critical-only and toggle on/off", () => {
    render(<SettingsPreferences />);
    // Enable audible first so the fieldset isn't disabled.
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: /play a tone when an alert fires/i,
      }),
    );
    const critical = screen.getByRole("checkbox", { name: /critical/i });
    const warning = screen.getByRole("checkbox", { name: /warning/i });
    const info = screen.getByRole("checkbox", { name: /info/i });
    expect(critical).toBeChecked();
    expect(warning).not.toBeChecked();
    expect(info).not.toBeChecked();

    fireEvent.click(warning);
    expect(getPreferences().audibleSeverities).toEqual([
      "critical",
      "warning",
    ]);

    fireEvent.click(critical);
    expect(getPreferences().audibleSeverities).toEqual(["warning"]);
  });

  it("severity fieldset is disabled when audible is off", () => {
    render(<SettingsPreferences />);
    const critical = screen.getByRole("checkbox", { name: /critical/i });
    expect(critical).toBeDisabled();
  });

  it("reflects changes made outside the component (live via store)", () => {
    render(<SettingsPreferences />);
    act(() => {
      setPreferences({
        theme: "dark",
        audibleEnabled: true,
        audibleSeverities: ["warning", "critical"],
      });
    });
    expect(screen.getByRole("radio", { name: /dark/i })).toBeChecked();
    expect(
      screen.getByRole("checkbox", {
        name: /play a tone when an alert fires/i,
      }),
    ).toBeChecked();
    const warning = screen.getByRole("checkbox", { name: /warning/i });
    expect(warning).toBeChecked();
  });
});
