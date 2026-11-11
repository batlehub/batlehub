import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { registryHealthMock, listNamespacesMock, claimMock, releaseMock, listSubjectsMock } =
  vi.hoisted(() => ({
    registryHealthMock: vi.fn(),
    listNamespacesMock: vi.fn(),
    claimMock: vi.fn(),
    releaseMock: vi.fn(),
    listSubjectsMock: vi.fn(),
  }));
vi.mock("@/client/sdk.gen", () => ({
  registryHealth: registryHealthMock,
  listNamespaces: listNamespacesMock,
  claimNamespace: claimMock,
  releaseNamespace: releaseMock,
  listSubjects: listSubjectsMock,
  explorePackages: vi.fn(),
  explorePackageDetail: vi.fn(),
}));

import AdminTeamNamespaces from "./AdminTeamNamespaces.vue";
import { Select } from "@/components/ui/select";

const ns = (over: Record<string, unknown> = {}) => ({
  registry: "cargo",
  prefix: "frontend",
  group_id: "team-fe",
  claimed_by: "oidc:alice",
  package_count: 42,
  ...over,
});

/** Never resolves — the state the empty message must not be shown in. */
const pending = () => new Promise(() => {});

async function mountPage() {
  const wrapper = mount(AdminTeamNamespaces, {
    global: { stubs: { SectionTabs: true, RouterLink: true } },
  });
  await flushPromises();
  return wrapper;
}

type Page = Awaited<ReturnType<typeof mountPage>>;

/**
 * The dialogs are teleported, so their controls are reached through the body.
 *
 * Scoped to the open dialog rather than to every button on the page: the page
 * renders "Claim namespace" three times outside the dialog, so a body-wide
 * search returned the *page's* button and clicked the wrong one the moment a
 * test used `attachTo`.
 */
const inDialog = (label: RegExp) => {
  const scope = document.querySelector('[role="dialog"]') ?? document.body;
  const button = Array.from(scope.querySelectorAll("button")).find((b) =>
    label.test((b.textContent ?? "").trim()),
  );
  expect(button, `no button matching ${label} in the open dialog`).toBeTruthy();
  return button!;
};

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

/** Open the claim dialog from the page header. */
async function openClaim(wrapper: Page) {
  await wrapper
    .findAll("button")
    .find((b) => /claim namespace/i.test(b.text()))!
    .trigger("click");
  await flushPromises();
}

const rowButton = (wrapper: Page, label: RegExp, row = 0) => {
  const rows = wrapper.findAll("tbody tr");
  return rows[row].findAll("button").find((b) => label.test(b.text()))!;
};

/**
 * The page's question: "which prefixes are claimed, by whom, and how much is
 * in them".
 */
describe("AdminTeamNamespaces", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  beforeEach(() => {
    registryHealthMock.mockReset().mockResolvedValue({
      data: [{ registry: "cargo", registry_type: "cargo" }],
    });
    listNamespacesMock.mockReset().mockResolvedValue({ data: [ns()] });
    claimMock.mockReset().mockResolvedValue({ data: {} });
    releaseMock.mockReset().mockResolvedValue({ data: {} });
    listSubjectsMock.mockReset().mockResolvedValue({ data: { items: [], truncated: false } });
  });

  /**
   * A6: `count_packages_in_namespace` had been on the port since it was written
   * with the *delete confirmation* as its only caller, so this list could not
   * tell a namespace holding four hundred packages from an abandoned one.
   */
  it("states how many packages each claim holds", async () => {
    listNamespacesMock.mockResolvedValue({
      data: [
        ns({ prefix: "frontend", package_count: 42 }),
        ns({ prefix: "dead", package_count: 0 }),
      ],
    });
    const wrapper = await mountPage();
    const counts = wrapper.findAll("tbody tr").map((row) => row.findAll("td").at(-2)!.text());
    // Zero is the answer that matters here, so it is rendered rather than blanked.
    expect(counts).toEqual(["42", "0"]);
  });

  /**
   * §4.3: "none for this registry" must never render while loading.
   *
   * It did, so an operator read the empty state, opened the dialog and claimed
   * a prefix that already existed. Empty must mean empty, not "we have not
   * looked yet".
   */
  it("does not claim the registry is empty while it is still loading", async () => {
    listNamespacesMock.mockReturnValue(pending());
    const wrapper = await mountPage();
    expect(wrapper.text()).not.toMatch(/no namespaces|none for this registry/i);
  });

  it("says so when a registry genuinely has no claims", async () => {
    listNamespacesMock.mockResolvedValue({ data: [] });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/no namespace/i);
  });

  it("surfaces a load error rather than an empty list", async () => {
    listNamespacesMock.mockResolvedValue({ error: { message: "cannot reach db" } });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("cannot reach db");
  });

  it("says a claim belongs to nobody in particular rather than leaving a blank", async () => {
    listNamespacesMock.mockResolvedValue({ data: [ns({ claimed_by: null })] });
    const wrapper = await mountPage();
    expect(wrapper.find("tbody tr").findAll("td")[2].text()).toBe("—");
  });

  it("asks for a registry before it can claim anything in one", async () => {
    registryHealthMock.mockResolvedValue({ data: [] });
    const wrapper = await mountPage();

    expect(
      wrapper
        .findAll("button")
        .find((b) => /claim namespace/i.test(b.text()))!
        .attributes("disabled"),
    ).toBeDefined();
    expect(wrapper.text()).toMatch(/select a registry/i);
  });

  it("lists the claims of whichever registry is selected", async () => {
    registryHealthMock.mockResolvedValue({
      data: [
        { registry: "cargo", registry_type: "cargo" },
        { registry: "npm", registry_type: "npm" },
      ],
    });
    const wrapper = await mountPage();
    expect(listNamespacesMock).toHaveBeenLastCalledWith({ path: { registry: "cargo" } });

    // Driven through the picker's own `v-model` contract rather than by
    // assigning `wrapper.vm.selectedRegistry`: the page's only way to change
    // registry is this control, and setting the ref behind it meant deleting
    // the `<Select>` from the template — or breaking its binding — left every
    // test in this file green while the page lost the control entirely.
    const picker = wrapper
      .findAllComponents(Select)
      .find((s) => s.props("id") === "team-ns-registry");
    expect(picker, "the registry picker is not rendered").toBeTruthy();
    expect(picker!.props("modelValue")).toBe("cargo");

    picker!.vm.$emit("update:modelValue", "npm");
    await flushPromises();

    expect(listNamespacesMock).toHaveBeenLastCalledWith({ path: { registry: "npm" } });
  });

  it("re-reads the claims on demand", async () => {
    const wrapper = await mountPage();
    expect(listNamespacesMock).toHaveBeenCalledTimes(1);

    await wrapper
      .findAll("button")
      .find((b) => /^Refresh$/.test(b.text()))!
      .trigger("click");
    await flushPromises();

    expect(listNamespacesMock).toHaveBeenCalledTimes(2);
  });

  // ── Claiming ────────────────────────────────────────────────────────────────

  it("will not claim without both a prefix and a group", async () => {
    const wrapper = await mountPage();
    await openClaim(wrapper);

    expect(inDialog(/^Claim namespace$/).hasAttribute("disabled")).toBe(true);

    await fill("#team-ns-prefix", "frontend");
    expect(inDialog(/^Claim namespace$/).hasAttribute("disabled")).toBe(true);

    await fill("#team-ns-group-id", "oidc:frontend-team");
    expect(inDialog(/^Claim namespace$/).hasAttribute("disabled")).toBe(false);
  });

  it("claims a prefix for a group, trimming what was typed", async () => {
    const wrapper = await mountPage();
    await openClaim(wrapper);

    await fill("#team-ns-prefix", "  frontend/ui  ");
    await fill("#team-ns-group-id", "  oidc:frontend-team  ");
    await clickInDialog(/^Claim namespace$/);

    expect(claimMock).toHaveBeenCalledWith({
      path: { registry: "cargo" },
      body: {
        prefix: "frontend/ui",
        group_id: "oidc:frontend-team",
        // Optional, and an empty box means "not stated" rather than "".
        claimed_by: undefined,
      },
    });
    // The list is re-read, so the new claim is on screen without a manual refresh.
    expect(listNamespacesMock).toHaveBeenCalledTimes(2);
    expect(document.querySelector("#team-ns-prefix")).toBeNull();
  });

  it("keeps the dialog open and says why when the claim is refused", async () => {
    claimMock.mockResolvedValue({ error: { message: "prefix already claimed by team-be" } });
    const wrapper = await mountPage();
    await openClaim(wrapper);

    await fill("#team-ns-prefix", "frontend");
    await fill("#team-ns-group-id", "oidc:frontend-team");
    await clickInDialog(/^Claim namespace$/);

    expect(document.body.textContent).toContain("prefix already claimed by team-be");
    expect(document.querySelector("#team-ns-prefix")).not.toBeNull();
  });

  it("abandons a claim on cancel", async () => {
    const wrapper = await mountPage();
    await openClaim(wrapper);
    await fill("#team-ns-prefix", "frontend");

    await clickInDialog(/^Cancel$/);

    expect(claimMock).not.toHaveBeenCalled();
    expect(document.querySelector("#team-ns-prefix")).toBeNull();
  });

  // ── Releasing ───────────────────────────────────────────────────────────────

  /**
   * Releasing a claim reopens a prefix anyone may publish under, so the dialog
   * names both the prefix and the group that is losing it.
   */
  it("names what a release gives up, then releases it", async () => {
    const wrapper = await mountPage();

    await rowButton(wrapper, /release/i).trigger("click");
    await flushPromises();

    expect(document.body.textContent).toContain("frontend");
    expect(document.body.textContent).toContain("team-fe");

    await clickInDialog(/^release$/i);

    expect(releaseMock).toHaveBeenCalledWith({
      path: { registry: "cargo", prefix: "frontend" },
    });
    expect(listNamespacesMock).toHaveBeenCalledTimes(2);
  });

  it("passes a slashed prefix through verbatim", async () => {
    listNamespacesMock.mockResolvedValue({ data: [ns({ prefix: "frontend/ui" })] });
    const wrapper = await mountPage();

    await rowButton(wrapper, /release/i).trigger("click");
    await flushPromises();
    await clickInDialog(/^release$/i);

    expect(releaseMock).toHaveBeenCalledWith({
      path: { registry: "cargo", prefix: "frontend/ui" },
    });
  });

  it("reports a refused release rather than closing on a lie", async () => {
    releaseMock.mockResolvedValue({ error: { message: "namespace still holds packages" } });
    const wrapper = await mountPage();

    await rowButton(wrapper, /release/i).trigger("click");
    await flushPromises();
    await clickInDialog(/^release$/i);

    expect(document.body.textContent).toContain("namespace still holds packages");
  });

  it("keeps the claim when the release is cancelled", async () => {
    const wrapper = await mountPage();

    await rowButton(wrapper, /release/i).trigger("click");
    await flushPromises();
    await clickInDialog(/^Cancel$/);

    expect(releaseMock).not.toHaveBeenCalled();
    expect(wrapper.findAll("tbody tr")).toHaveLength(1);
  });
});
