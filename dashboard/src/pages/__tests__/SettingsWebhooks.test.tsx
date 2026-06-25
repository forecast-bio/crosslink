// Coverage for the /settings/webhooks page. Mocks useWebhooks +
// useSetWebhooks so we can drive the draft/save flow without hitting
// the real endpoints.

import "@testing-library/jest-dom/vitest";
import { describe, expect, it, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import type { WebhooksView } from "@/api/types";

const stubMutation = <TData, TVars>() => ({
  mutate: vi.fn<(vars: TVars) => void>(),
  mutateAsync: vi.fn(),
  reset: vi.fn(),
  isPending: false,
  isSuccess: false,
  isError: false,
  error: null as Error | null,
  data: undefined as TData | undefined,
});

const stubQuery = <T,>(
  data: T,
  overrides: Partial<{ isLoading: boolean; error: Error | null }> = {},
) => ({
  data,
  isLoading: false,
  isFetching: false,
  isError: false,
  error: null,
  refetch: vi.fn(),
  ...overrides,
});

const mocks = {
  useWebhooks: vi.fn(),
  useSetWebhooks: vi.fn(),
};

vi.mock("@/api/client", async () => {
  const actual = await vi.importActual<typeof import("@/api/client")>(
    "@/api/client",
  );
  return {
    ...actual,
    useWebhooks: () => mocks.useWebhooks(),
    useSetWebhooks: () => mocks.useSetWebhooks(),
  };
});

function withClient(ui: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return <QueryClientProvider client={client}>{ui}</QueryClientProvider>;
}

const emptyView: WebhooksView = { urls: [] };
const populatedView: WebhooksView = {
  urls: [
    "https://hooks.slack.com/services/T/B/XYZ",
    "https://discord.com/api/webhooks/1/abc",
  ],
};

beforeEach(() => {
  mocks.useWebhooks.mockReturnValue(stubQuery(emptyView));
  mocks.useSetWebhooks.mockReturnValue(stubMutation());
});

describe("SettingsWebhooks page", () => {
  it("shows empty-state message when no webhooks configured", async () => {
    const { SettingsWebhooks } = await import("../SettingsWebhooks");
    render(withClient(<SettingsWebhooks />));
    expect(screen.getByText(/no webhooks configured/i)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /saved/i }),
    ).toBeDisabled();
  });

  it("renders existing webhooks from the server", async () => {
    mocks.useWebhooks.mockReturnValue(stubQuery(populatedView));
    const { SettingsWebhooks } = await import("../SettingsWebhooks");
    render(withClient(<SettingsWebhooks />));

    expect(
      screen.getByText("https://hooks.slack.com/services/T/B/XYZ"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("https://discord.com/api/webhooks/1/abc"),
    ).toBeInTheDocument();
  });

  it("adding a new URL enables Save and then calls setWebhooks with the full list", async () => {
    const save = stubMutation<WebhooksView, { urls: string[] }>();
    mocks.useSetWebhooks.mockReturnValue(save);
    const { SettingsWebhooks } = await import("../SettingsWebhooks");
    render(withClient(<SettingsWebhooks />));

    fireEvent.change(screen.getByPlaceholderText(/hooks\.slack\.com/i), {
      target: { value: "  https://example.com/hook  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    const saveBtn = screen.getByRole("button", { name: /save changes/i });
    expect(saveBtn).not.toBeDisabled();

    fireEvent.click(saveBtn);
    expect(save.mutate).toHaveBeenCalledWith({
      urls: ["https://example.com/hook"],
    });
  });

  it("dedupes entries on add", async () => {
    mocks.useWebhooks.mockReturnValue(
      stubQuery({ urls: ["https://hooks.slack.com/services/T/B/XYZ"] }),
    );
    const { SettingsWebhooks } = await import("../SettingsWebhooks");
    render(withClient(<SettingsWebhooks />));

    fireEvent.change(screen.getByPlaceholderText(/hooks\.slack\.com/i), {
      target: { value: "https://hooks.slack.com/services/T/B/XYZ" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    // Still one row rendered (the existing one), no duplicate appended.
    const items = screen.getAllByRole("listitem");
    expect(items).toHaveLength(1);
  });

  it("removing an entry dirties the list and includes the remaining URL on save", async () => {
    const save = stubMutation<WebhooksView, { urls: string[] }>();
    mocks.useSetWebhooks.mockReturnValue(save);
    mocks.useWebhooks.mockReturnValue(stubQuery(populatedView));
    const { SettingsWebhooks } = await import("../SettingsWebhooks");
    render(withClient(<SettingsWebhooks />));

    fireEvent.click(
      screen.getByRole("button", {
        name: /remove https:\/\/hooks\.slack\.com/i,
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: /save changes/i }));

    expect(save.mutate).toHaveBeenCalledWith({
      urls: ["https://discord.com/api/webhooks/1/abc"],
    });
  });

  it("surfaces the server error message on save failure", async () => {
    const save = stubMutation<WebhooksView, { urls: string[] }>();
    save.error = new Error("ftp://bad: unsupported scheme");
    mocks.useSetWebhooks.mockReturnValue(save);
    const { SettingsWebhooks } = await import("../SettingsWebhooks");
    render(withClient(<SettingsWebhooks />));
    expect(
      screen.getByText(/ftp:\/\/bad: unsupported scheme/i),
    ).toBeInTheDocument();
  });

  it("surfaces the load error when the GET fails", async () => {
    mocks.useWebhooks.mockReturnValue(
      stubQuery(undefined as WebhooksView | undefined, {
        error: new Error("server exploded"),
      }),
    );
    const { SettingsWebhooks } = await import("../SettingsWebhooks");
    render(withClient(<SettingsWebhooks />));
    expect(screen.getByText(/server exploded/i)).toBeInTheDocument();
  });
});

// Silence an unused `act` import warning if future refactors drop it.
void act;
