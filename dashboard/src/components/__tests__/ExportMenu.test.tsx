// Smoke coverage for the ExportMenu component. The download helper
// is mocked so we can assert the button wiring without touching the
// browser's download machinery.

import "@testing-library/jest-dom/vitest";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

const download = vi.fn(async (_opts: { path: string; filename: string }) => undefined);

vi.mock("@/lib/download", () => ({
  downloadAuthenticated: (opts: { path: string; filename: string }) =>
    download(opts),
}));

describe("ExportMenu", () => {
  beforeEach(() => {
    download.mockClear();
    download.mockResolvedValue(undefined);
  });

  it("CSV button calls download with .csv path + filename", async () => {
    const { ExportMenu } = await import("../ExportMenu");
    render(
      <ExportMenu
        label="projects"
        pathPrefix="/export/projects"
        filenameStem="crosslink-projects"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /csv/i }));
    await waitFor(() => {
      expect(download).toHaveBeenCalledWith({
        path: "/export/projects.csv",
        filename: "crosslink-projects.csv",
      });
    });
  });

  it("JSON button calls download with .json path + filename", async () => {
    const { ExportMenu } = await import("../ExportMenu");
    render(
      <ExportMenu
        label="alerts"
        pathPrefix="/export/alerts"
        filenameStem="crosslink-alerts"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /json/i }));
    await waitFor(() => {
      expect(download).toHaveBeenCalledWith({
        path: "/export/alerts.json",
        filename: "crosslink-alerts.json",
      });
    });
  });

  it("surfaces server error next to the buttons", async () => {
    download.mockRejectedValueOnce(new Error("dashboard DB not configured"));
    const { ExportMenu } = await import("../ExportMenu");
    render(
      <ExportMenu
        label="projects"
        pathPrefix="/export/projects"
        filenameStem="crosslink-projects"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /csv/i }));
    await waitFor(() => {
      expect(screen.getByText(/dashboard DB not configured/)).toBeInTheDocument();
    });
  });
});
