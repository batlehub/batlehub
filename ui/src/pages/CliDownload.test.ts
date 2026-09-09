import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";
import { ref } from "vue";

const auth = { token: ref("tok-123") };
vi.mock("@/composables/useAuth", () => ({ useAuth: () => auth }));

import CliDownload from "./CliDownload.vue";

const stubs = { CopyButton: { name: "CopyButton", props: ["text"], template: "<span/>" } };
const mountPage = () => mount(CliDownload, { global: { stubs } });

const fetchMock = vi.fn();
let clicked: HTMLAnchorElement[] = [];

beforeEach(() => {
  auth.token.value = "tok-123";
  clicked = [];
  fetchMock.mockReset();
  globalThis.fetch = fetchMock as unknown as typeof fetch;
  URL.createObjectURL = vi.fn(() => "blob:cli");
  URL.revokeObjectURL = vi.fn();
  // jsdom cannot follow a download, so the anchor is captured instead of clicked.
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(
    function (this: HTMLAnchorElement) {
      clicked.push(this);
    },
  );
});

afterEach(() => vi.restoreAllMocks());

async function download(wrapper: ReturnType<typeof mountPage>) {
  await wrapper
    .findAll("button")
    .find((b) => /download/i.test(b.text()))!
    .trigger("click");
  await flushPromises();
}

/**
 * `/me/cli`: the page an operator lands on to get the binary and the config
 * file that points it at this server.
 *
 * The download runs through `fetch` rather than a plain link because the
 * endpoint is authenticated — so the interesting behaviour is what the page
 * does when that request fails, which a link could only ever show as a blank
 * tab.
 */
describe("CliDownload", () => {
  it("saves the binary under the filename the server names", async () => {
    fetchMock.mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'attachment; filename="batlehub-cli-linux-amd64"' },
      blob: async () => new Blob(["binary"]),
    });
    const w = mountPage();
    await download(w);

    expect(fetchMock).toHaveBeenCalledWith(
      expect.stringContaining("/api/v1/cli/download"),
      // The bearer token goes with it: the endpoint is not anonymous.
      { headers: { Authorization: "Bearer tok-123" } },
    );
    expect(clicked).toHaveLength(1);
    expect(clicked[0].download).toBe("batlehub-cli-linux-amd64");
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:cli");
  });

  it("falls back to a plain filename when the server sends no disposition", async () => {
    fetchMock.mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => null },
      blob: async () => new Blob(["binary"]),
    });
    const w = mountPage();
    await download(w);
    expect(clicked[0].download).toBe("batlehub-cli");
  });

  it("sends no Authorization header when there is no token", async () => {
    auth.token.value = "";
    fetchMock.mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => null },
      blob: async () => new Blob(["binary"]),
    });
    await download(mountPage());
    expect(fetchMock).toHaveBeenCalledWith(expect.any(String), { headers: {} });
  });

  /**
   * A `404` here means the instance ships no binary — a deployment choice, not
   * a failure — so it gets its own sentence rather than a status code.
   */
  it("explains a 404 as a binary this instance does not serve", async () => {
    fetchMock.mockResolvedValue({ ok: false, status: 404, text: async () => "" });
    const w = mountPage();
    await download(w);
    expect(w.text()).toMatch(/has not been configured/i);
    expect(clicked).toHaveLength(0);
  });

  it("reports any other failure with its status and the server's detail", async () => {
    fetchMock.mockResolvedValue({ ok: false, status: 500, text: async () => "storage down" });
    const w = mountPage();
    await download(w);
    expect(w.text()).toContain("500");
    expect(w.text()).toContain("storage down");
  });

  it("reports a network failure rather than staying on Downloading", async () => {
    fetchMock.mockRejectedValue(new Error("connection refused"));
    const w = mountPage();
    await download(w);
    expect(w.text()).toContain("connection refused");
    // The button is usable again: a stuck spinner would strand the page.
    const button = w.findAll("button").find((b) => /download/i.test(b.text()))!;
    expect(button.attributes("disabled")).toBeUndefined();
  });

  it("offers install snippets for every platform it claims to support", () => {
    const w = mountPage();
    const text = w.text();
    for (const label of [
      "mise",
      "Linux x86_64",
      "Linux aarch64",
      "macOS Apple Silicon",
      "Windows",
    ]) {
      expect(text).toContain(label);
    }
  });

  it("writes this server's origin into the config snippet", () => {
    const snippets = mountPage()
      .findAllComponents({ name: "CopyButton" })
      .map((c) => c.props("text") as string);
    const config = snippets.find((s) => s.includes("server_url"))!;
    expect(config).toContain(globalThis.location.origin);
    expect(config).toContain("token");
  });

  it("lists the commands a new user runs first", () => {
    const text = mountPage().text();
    expect(text).toContain("batlehub-cli registry list");
    expect(text).toContain("batlehub-cli auth whoami");
  });
});
