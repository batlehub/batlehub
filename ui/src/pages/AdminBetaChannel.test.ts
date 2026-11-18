import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { registryHealthMock, listMembersMock, addMock, removeMock, listSubjectsMock } = vi.hoisted(
  () => ({
    registryHealthMock: vi.fn(),
    listMembersMock: vi.fn(),
    addMock: vi.fn(),
    removeMock: vi.fn(),
    listSubjectsMock: vi.fn(),
  }),
);
vi.mock("@/client/sdk.gen", () => ({
  registryHealth: registryHealthMock,
  listBetaMembers: listMembersMock,
  addBetaMember: addMock,
  removeBetaMember: removeMock,
  listSubjects: listSubjectsMock,
  explorePackages: vi.fn(),
  explorePackageDetail: vi.fn(),
}));

import AdminBetaChannel from "./AdminBetaChannel.vue";

const member = (over: Record<string, unknown> = {}) => ({
  registry: "npm",
  principal_type: "user",
  principal_id: "oidc:alice",
  granted_by: "admin",
  granted_at: "2026-08-01T00:00:00Z",
  ...over,
});

const pending = () => new Promise(() => {});

async function mountPage() {
  const wrapper = mount(AdminBetaChannel, {
    global: { stubs: { RouterLink: true } },
  });
  await flushPromises();
  return wrapper;
}

type Page = Awaited<ReturnType<typeof mountPage>>;

/** Both dialogs are teleported, so their controls are reached through the body. */
const inDialog = (label: RegExp) =>
  Array.from(document.querySelectorAll("button")).find((b) =>
    label.test((b.textContent ?? "").trim()),
  )!;

async function clickInDialog(label: RegExp) {
  inDialog(label).click();
  await flushPromises();
}

async function fill(selector: string, value: string) {
  const el = document.querySelector<HTMLInputElement>(selector)!;
  el.value = value;
  el.dispatchEvent(new Event("input"));
  await flushPromises();
}

async function openAdd(wrapper: Page) {
  await wrapper
    .findAll("button")
    .find((b) => /add member/i.test(b.text()))!
    .trigger("click");
  await flushPromises();
}

/** The page's question: "who is on the beta channel for this registry". */
describe("AdminBetaChannel", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  beforeEach(() => {
    registryHealthMock.mockReset().mockResolvedValue({
      data: [{ registry: "npm", registry_type: "npm" }],
    });
    listMembersMock.mockReset().mockResolvedValue({ data: [member()] });
    addMock.mockReset().mockResolvedValue({ data: {} });
    removeMock.mockReset().mockResolvedValue({ data: {} });
    listSubjectsMock.mockReset().mockResolvedValue({ data: { items: [], truncated: false } });
  });

  it("lists the members of the selected registry", async () => {
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("oidc:alice");
  });

  /**
   * §4.3: "none for this registry" must never render while loading — the same
   * defect as `AdminTeamNamespaces`, on the page beside it. An operator who
   * reads an empty state that means "not yet fetched" grants access twice.
   */
  it("does not claim the channel is empty while it is still loading", async () => {
    listMembersMock.mockReturnValue(pending());
    const wrapper = await mountPage();
    expect(wrapper.text()).not.toMatch(/no beta channel members/i);
  });

  it("says so when the channel genuinely has no members", async () => {
    listMembersMock.mockResolvedValue({ data: [] });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/no beta channel members/i);
  });

  it("surfaces a load error rather than an empty channel", async () => {
    listMembersMock.mockResolvedValue({ error: { message: "cannot reach db" } });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("cannot reach db");
  });

  /** A user and a group are different kinds of grant, and the row says which. */
  it("distinguishes a user grant from a group grant", async () => {
    listMembersMock.mockResolvedValue({
      data: [
        member({ principal_type: "user", principal_id: "oidc:alice" }),
        member({ principal_type: "group", principal_id: "team-frontend" }),
      ],
    });
    const wrapper = await mountPage();
    const badges = wrapper.findAll("tbody tr").map((r) => r.find("td").html());

    expect(badges[0]).toContain("user");
    expect(badges[1]).toContain("group");
    expect(badges[0]).not.toEqual(badges[1]);
  });

  it("says nobody is on record as having granted access rather than leaving a blank", async () => {
    listMembersMock.mockResolvedValue({ data: [member({ granted_by: null })] });
    const wrapper = await mountPage();
    expect(wrapper.find("tbody tr").findAll("td")[2].text()).toBe("—");
  });

  it("asks for a registry before it can grant access in one", async () => {
    registryHealthMock.mockResolvedValue({ data: [] });
    const wrapper = await mountPage();

    expect(
      wrapper
        .findAll("button")
        .find((b) => /add member/i.test(b.text()))!
        .attributes("disabled"),
    ).toBeDefined();
    expect(wrapper.text()).toMatch(/select a registry/i);
  });

  it("lists the members of whichever registry is selected", async () => {
    registryHealthMock.mockResolvedValue({
      data: [
        { registry: "npm", registry_type: "npm" },
        { registry: "pypi", registry_type: "pypi" },
      ],
    });
    const wrapper = await mountPage();
    expect(listMembersMock).toHaveBeenLastCalledWith({ path: { registry: "npm" } });

    (wrapper.vm as unknown as { selectedRegistry: string }).selectedRegistry = "pypi";
    await flushPromises();

    expect(listMembersMock).toHaveBeenLastCalledWith({ path: { registry: "pypi" } });
  });

  it("re-reads the members on demand", async () => {
    const wrapper = await mountPage();
    await wrapper
      .findAll("button")
      .find((b) => /^Refresh$/.test(b.text()))!
      .trigger("click");
    await flushPromises();

    expect(listMembersMock).toHaveBeenCalledTimes(2);
  });

  // ── Granting ────────────────────────────────────────────────────────────────

  it("will not grant access to nobody", async () => {
    const wrapper = await mountPage();
    await openAdd(wrapper);

    expect(inDialog(/^Add member$/).hasAttribute("disabled")).toBe(true);

    await fill("#beta-principal-id", "alice");
    expect(inDialog(/^Add member$/).hasAttribute("disabled")).toBe(false);
  });

  it("adds a member, trimming what was typed", async () => {
    const wrapper = await mountPage();
    await openAdd(wrapper);

    await fill("#beta-principal-id", "  alice  ");
    await clickInDialog(/^Add member$/);

    expect(addMock).toHaveBeenCalledWith({
      path: { registry: "npm" },
      body: {
        principal_type: "user",
        principal_id: "alice",
        // Optional, and an empty box means "not stated" rather than "".
        granted_by: undefined,
      },
    });
    expect(listMembersMock).toHaveBeenCalledTimes(2);
    expect(document.querySelector("#beta-principal-id")).toBeNull();
  });

  it("adds a group as easily as a user", async () => {
    const wrapper = await mountPage();
    await openAdd(wrapper);

    (wrapper.vm as unknown as { addForm: { principal_type: string } }).addForm.principal_type =
      "group";
    await fill("#beta-principal-id", "team-frontend");
    await clickInDialog(/^Add member$/);

    expect(addMock).toHaveBeenCalledWith({
      path: { registry: "npm" },
      body: { principal_type: "group", principal_id: "team-frontend", granted_by: undefined },
    });
  });

  it("keeps the dialog open and says why when the grant is refused", async () => {
    addMock.mockResolvedValue({ error: { message: "principal already a member" } });
    const wrapper = await mountPage();
    await openAdd(wrapper);

    await fill("#beta-principal-id", "alice");
    await clickInDialog(/^Add member$/);

    expect(document.body.textContent).toContain("principal already a member");
    expect(document.querySelector("#beta-principal-id")).not.toBeNull();
  });

  it("abandons a grant on cancel", async () => {
    const wrapper = await mountPage();
    await openAdd(wrapper);
    await fill("#beta-principal-id", "alice");

    await clickInDialog(/^Cancel$/);

    expect(addMock).not.toHaveBeenCalled();
    expect(document.querySelector("#beta-principal-id")).toBeNull();
  });

  // ── Revoking ────────────────────────────────────────────────────────────────

  it("names who loses pre-release access, then removes them", async () => {
    const wrapper = await mountPage();

    await wrapper
      .find("tbody tr")
      .findAll("button")
      .find((b) => /remove/i.test(b.text()))!
      .trigger("click");
    await flushPromises();

    expect(document.body.textContent).toContain("oidc:alice");
    expect(document.body.textContent).toContain("npm");

    await clickInDialog(/^Remove$/);

    expect(removeMock).toHaveBeenCalledWith({
      path: { registry: "npm", principal_type: "user", principal_id: "oidc:alice" },
    });
    expect(listMembersMock).toHaveBeenCalledTimes(2);
  });

  it("reports a refused removal rather than closing on a lie", async () => {
    removeMock.mockResolvedValue({ error: { message: "grant is managed by the auth provider" } });
    const wrapper = await mountPage();

    await wrapper
      .find("tbody tr")
      .findAll("button")
      .find((b) => /remove/i.test(b.text()))!
      .trigger("click");
    await flushPromises();
    await clickInDialog(/^Remove$/);

    expect(document.body.textContent).toContain("grant is managed by the auth provider");
  });

  it("keeps the member when the removal is cancelled", async () => {
    const wrapper = await mountPage();

    await wrapper
      .find("tbody tr")
      .findAll("button")
      .find((b) => /remove/i.test(b.text()))!
      .trigger("click");
    await flushPromises();
    await clickInDialog(/^Cancel$/);

    expect(removeMock).not.toHaveBeenCalled();
    expect(wrapper.findAll("tbody tr")).toHaveLength(1);
  });
});
