import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { nextTick } from "vue";

const { meMock, oidcRefreshMock, setConfigMock } = vi.hoisted(() => ({
  meMock: vi.fn(),
  oidcRefreshMock: vi.fn(),
  setConfigMock: vi.fn(),
}));

vi.mock("@/client/sdk.gen", () => ({
  me: meMock,
  oidcRefresh: oidcRefreshMock,
}));
vi.mock("@/client/client.gen", () => ({
  client: { setConfig: setConfigMock },
}));

import { storeTokens, useAuth } from "./useAuth";

const ANON = { role: "anonymous" as const, groups: [], has_registry_access: false };
const ADMIN = { role: "admin" as const, groups: ["admins"], has_registry_access: true };

describe("useAuth", () => {
  beforeEach(() => {
    meMock.mockReset().mockResolvedValue({ data: ANON });
    oidcRefreshMock.mockReset();
    setConfigMock.mockClear();
    useAuth().logout();
    localStorage.clear();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("storeTokens persists tokens to localStorage and configures the API client", async () => {
    storeTokens("access1", "refresh1", 3600);
    await nextTick();

    expect(localStorage.getItem("batlehub_access_token")).toBe("access1");
    expect(localStorage.getItem("batlehub_refresh_token")).toBe("refresh1");
    expect(setConfigMock).toHaveBeenCalledWith({ auth: "access1" });
  });

  it("calls a session OIDC only when the OIDC callback named its provider", async () => {
    const { isOidcSession } = useAuth();
    storeTokens("static-tok"); // the login form's pasted token
    expect(isOidcSession.value).toBe(false);
    storeTokens("jwt", "refresh", 3600, "keycloak"); // the OIDC callback
    expect(isOidcSession.value).toBe(true);
  });

  it("fetches the identity and exposes isAuthenticated/isAdmin for an admin", async () => {
    meMock.mockResolvedValue({ data: ADMIN });
    const { identity, isAuthenticated, isAdmin } = useAuth();

    storeTokens("access1", "refresh1", 3600);
    await vi.waitFor(() => expect(identity.value).toEqual(ADMIN));

    expect(isAuthenticated.value).toBe(true);
    expect(isAdmin.value).toBe(true);
  });

  it("treats an anonymous identity as not authenticated", async () => {
    meMock.mockResolvedValue({ data: ANON });
    const { identity, isAuthenticated, isAdmin } = useAuth();

    storeTokens("access1", "refresh1", 3600);
    await vi.waitFor(() => expect(identity.value).toEqual(ANON));

    expect(isAuthenticated.value).toBe(false);
    expect(isAdmin.value).toBe(false);
  });

  it("logout clears tokens, admin status and storage", async () => {
    meMock.mockResolvedValue({ data: ADMIN });
    const auth = useAuth();
    storeTokens("access1", "refresh1", 3600);
    await vi.waitFor(() => expect(auth.isAdmin.value).toBe(true));

    meMock.mockResolvedValue({ data: ANON });
    auth.logout();
    await vi.waitFor(() => expect(auth.isAdmin.value).toBe(false));

    expect(auth.token.value).toBe("");
    expect(localStorage.getItem("batlehub_access_token")).toBeNull();
  });

  it("doRefresh exchanges the refresh token for a new access token", async () => {
    storeTokens("access1", "refresh1", 3600);
    await nextTick();

    oidcRefreshMock.mockResolvedValue({
      data: { access_token: "access2", refresh_token: "refresh2", expires_in: 7200 },
    });

    await useAuth().doRefresh();

    expect(oidcRefreshMock).toHaveBeenCalledWith({
      body: { refresh_token: "refresh1", provider: undefined },
    });
    expect(useAuth().token.value).toBe("access2");
    expect(useAuth().refreshToken.value).toBe("refresh2");
  });

  it("schedules an automatic refresh ~60s before the token expires", async () => {
    vi.useFakeTimers();
    oidcRefreshMock.mockResolvedValue({
      data: { access_token: "access2", refresh_token: "refresh2", expires_in: 3600 },
    });

    storeTokens("access1", "refresh1", 3600);

    await vi.advanceTimersByTimeAsync(3_600_000 - 60_000);

    expect(oidcRefreshMock).toHaveBeenCalled();
  });
});
