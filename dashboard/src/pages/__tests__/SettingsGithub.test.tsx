// Coverage for the /settings/github page. Mocks the GitHub API hooks
// so we can drive the form and discovery flows without hitting the
// real `/api/v1/dashboard/github/*` endpoints or github.com.

import "@testing-library/jest-dom/vitest";
import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import type {
  CloneRepoOutcome,
  GithubConfigView,
  GithubRepoHit,
  GithubTrackAllOutcome,
} from "@/api/types";

const stubMutation = <TData, TVars>() => ({
  mutate: vi.fn<(vars: TVars, opts?: { onSuccess?: (data: TData) => void }) => void>(),
  mutateAsync: vi.fn(),
  reset: vi.fn(),
  isPending: false,
  isSuccess: false,
  isError: false,
  error: null as Error | null,
  data: undefined as TData | undefined,
});

const stubQuery = <T,>(data: T, overrides: Partial<{ isLoading: boolean; isFetching: boolean; error: Error | null }> = {}) => ({
  data,
  isLoading: false,
  isFetching: false,
  isError: false,
  error: null,
  refetch: vi.fn(),
  ...overrides,
});

const mocks = {
  useGithubConfig: vi.fn(),
  useSetGithubConfig: vi.fn(),
  useOrgRepos: vi.fn(),
  useTrackAllOrg: vi.fn(),
  useCloneRepo: vi.fn(),
};

vi.mock("@/api/client", async () => {
  const actual = await vi.importActual<typeof import("@/api/client")>("@/api/client");
  return {
    ...actual,
    useGithubConfig: () => mocks.useGithubConfig(),
    useSetGithubConfig: () => mocks.useSetGithubConfig(),
    useOrgRepos: (org: string | null, enabled: boolean) =>
      mocks.useOrgRepos(org, enabled),
    useTrackAllOrg: () => mocks.useTrackAllOrg(),
    useCloneRepo: () => mocks.useCloneRepo(),
  };
});

function withClient(ui: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return <QueryClientProvider client={client}>{ui}</QueryClientProvider>;
}

const emptyConfig: GithubConfigView = {
  token_present: false,
  token_fingerprint: null,
  default_org: null,
  token_source: null,
};

const populatedConfig: GithubConfigView = {
  token_present: true,
  token_fingerprint: "ghp_1234…wxyz",
  default_org: "my-org",
  token_source: "stored",
};

const repoHit: GithubRepoHit = {
  owner: "my-org",
  repo: "alpha",
  full_name: "my-org/alpha",
  default_branch: "main",
  ssh_url: "git@github.com:my-org/alpha.git",
  https_url: "https://github.com/my-org/alpha.git",
  has_hub_branch: true,
};

beforeEach(() => {
  mocks.useGithubConfig.mockReturnValue(stubQuery(emptyConfig));
  mocks.useSetGithubConfig.mockReturnValue(stubMutation());
  mocks.useOrgRepos.mockReturnValue(stubQuery<GithubRepoHit[] | undefined>(undefined));
  mocks.useTrackAllOrg.mockReturnValue(stubMutation());
  mocks.useCloneRepo.mockReturnValue(stubMutation());
});

describe("SettingsGithub page", () => {
  it("shows 'no token stored' when config empty", async () => {
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));
    expect(screen.getByText(/no token stored/i)).toBeInTheDocument();
  });

  it("shows masked fingerprint + remove button when token present", async () => {
    mocks.useGithubConfig.mockReturnValue(stubQuery(populatedConfig));
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));
    expect(screen.getByText("ghp_1234…wxyz")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /remove/i })).toBeInTheDocument();
  });

  it("saving a new token calls set-config with the trimmed value", async () => {
    const setMut = stubMutation();
    mocks.useSetGithubConfig.mockReturnValue(setMut);
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));

    fireEvent.change(screen.getByPlaceholderText(/ghp_/), {
      target: { value: "  ghp_paste_me  " },
    });
    fireEvent.click(screen.getByRole("button", { name: /save token/i }));

    expect(setMut.mutate).toHaveBeenCalledWith(
      { token: "ghp_paste_me" },
      expect.any(Object),
    );
  });

  it("saving an empty org sends null to clear it", async () => {
    const setMut = stubMutation();
    mocks.useSetGithubConfig.mockReturnValue(setMut);
    mocks.useGithubConfig.mockReturnValue(stubQuery(populatedConfig));
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));

    fireEvent.click(screen.getByRole("button", { name: /save org/i }));
    expect(setMut.mutate).toHaveBeenCalledWith({ default_org: null });
  });

  it("browse button enables repo enumeration for the default org", async () => {
    mocks.useGithubConfig.mockReturnValue(stubQuery(populatedConfig));
    mocks.useOrgRepos.mockReturnValue(stubQuery([repoHit]));
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));

    fireEvent.click(screen.getByRole("button", { name: /browse my-org/i }));
    expect(mocks.useOrgRepos).toHaveBeenLastCalledWith("my-org", true);
    expect(screen.getByText("my-org/alpha")).toBeInTheDocument();
  });

  it("track-all fires mutation with the current org + clone root", async () => {
    mocks.useGithubConfig.mockReturnValue(stubQuery(populatedConfig));
    mocks.useOrgRepos.mockReturnValue(stubQuery([repoHit]));
    const track = stubMutation<GithubTrackAllOutcome, { org: string; cloneRoot?: string }>();
    mocks.useTrackAllOrg.mockReturnValue(track);
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));

    fireEvent.click(screen.getByRole("button", { name: /browse my-org/i }));
    fireEvent.change(screen.getByPlaceholderText(/clone root/i), {
      target: { value: "/tmp/clones" },
    });
    fireEvent.click(screen.getByRole("button", { name: /track all 1/i }));

    expect(track.mutate).toHaveBeenCalledWith(
      {
        org: "my-org",
        cloneRoot: "/tmp/clones",
        init: false,
        agentId: undefined,
      },
      expect.any(Object),
    );
  });

  it("renders track outcome after a successful track-all", async () => {
    mocks.useGithubConfig.mockReturnValue(stubQuery(populatedConfig));
    mocks.useOrgRepos.mockReturnValue(stubQuery([repoHit]));
    const track = stubMutation<GithubTrackAllOutcome, { org: string; cloneRoot?: string }>();
    track.mutate = vi.fn((_vars, opts) => {
      opts?.onSuccess?.({
        tracked: ["my-org/alpha"],
        skipped: [{ slug: "my-org/beta", reason: "already tracked" }],
      });
    });
    mocks.useTrackAllOrg.mockReturnValue(track);

    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));

    fireEvent.click(screen.getByRole("button", { name: /browse my-org/i }));
    fireEvent.click(screen.getByRole("button", { name: /track all 1/i }));

    expect(screen.getByText(/tracked 1.*skipped 1/i)).toBeInTheDocument();
    expect(screen.getByText(/tracked \(1\)/i)).toBeInTheDocument();
    expect(screen.getByText(/skipped \(1\)/i)).toBeInTheDocument();
  });

  it("browse button is disabled until both token + org are configured", async () => {
    mocks.useGithubConfig.mockReturnValue(stubQuery({ ...emptyConfig, default_org: "my-org" }));
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));
    expect(screen.getByRole("button", { name: /browse my-org/i })).toBeDisabled();
  });

  it("Clone & track form posts URL + optional init/agentId", async () => {
    const clone = stubMutation<CloneRepoOutcome, { url: string; init?: boolean; agentId?: string }>();
    mocks.useCloneRepo.mockReturnValue(clone);
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));

    fireEvent.change(screen.getByLabelText(/clone url/i), {
      target: { value: "https://github.com/owner/repo.git" },
    });
    // Without init: slug undefined, init undefined — payload only has url.
    fireEvent.click(screen.getByRole("button", { name: /clone & track/i }));
    expect(clone.mutate).toHaveBeenCalledWith(
      { url: "https://github.com/owner/repo.git", slug: undefined, init: false, agentId: undefined },
      expect.any(Object),
    );
  });

  it("Clone & track form with init toggled requires agent id", async () => {
    const clone = stubMutation<CloneRepoOutcome, { url: string; init?: boolean; agentId?: string }>();
    mocks.useCloneRepo.mockReturnValue(clone);
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));

    fireEvent.change(screen.getByLabelText(/clone url/i), {
      target: { value: "https://github.com/owner/repo.git" },
    });
    fireEvent.click(
      screen.getByRole("checkbox", { name: /initialize after clone/i }),
    );
    // Without agent id: button disabled, warning shown.
    const btn = screen.getByRole("button", { name: /clone & track/i });
    expect(btn).toBeDisabled();
    expect(screen.getByText(/agent id required/i)).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText(/clone agent id/i), {
      target: { value: "my-agent" },
    });
    fireEvent.click(screen.getByRole("button", { name: /clone & track/i }));
    expect(clone.mutate).toHaveBeenCalledWith(
      {
        url: "https://github.com/owner/repo.git",
        slug: undefined,
        init: true,
        agentId: "my-agent",
      },
      expect.any(Object),
    );
  });

  it("Initialize checkbox reveals agent-id field and gates Track All", async () => {
    mocks.useGithubConfig.mockReturnValue(stubQuery(populatedConfig));
    mocks.useOrgRepos.mockReturnValue(stubQuery([repoHit]));
    const track = stubMutation<GithubTrackAllOutcome, { org: string; cloneRoot?: string; init?: boolean; agentId?: string }>();
    mocks.useTrackAllOrg.mockReturnValue(track);
    const { SettingsGithub } = await import("../SettingsGithub");
    render(withClient(<SettingsGithub />));

    fireEvent.click(screen.getByRole("button", { name: /browse my-org/i }));
    // Check the Initialize checkbox — agent-id field appears, Track All is disabled.
    fireEvent.click(screen.getByRole("checkbox", { name: /initialize cloned repos/i }));
    const agentField = screen.getByLabelText(/agent id/i);
    expect(agentField).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /track all 1/i })).toBeDisabled();
    expect(screen.getByText(/agent id required/i)).toBeInTheDocument();

    fireEvent.change(agentField, { target: { value: "dashboard-host" } });
    expect(screen.getByRole("button", { name: /track all 1/i })).not.toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /track all 1/i }));
    expect(track.mutate).toHaveBeenCalledWith(
      {
        org: "my-org",
        cloneRoot: undefined,
        init: true,
        agentId: "dashboard-host",
      },
      expect.any(Object),
    );
  });
});
