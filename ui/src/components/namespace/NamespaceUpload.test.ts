import { mount, flushPromises } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RegistryInfo } from "@/client/types.gen";

const { authFetchMock } = vi.hoisted(() => ({ authFetchMock: vi.fn() }));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));

import NamespaceUpload from "./NamespaceUpload.vue";

const reg = (name: string, type: string, mode = "local", public_url?: string) =>
  ({ name, type, mode, public_url }) as unknown as RegistryInfo;

const REGISTRIES = [
  reg("gems", "rubygems"),
  reg("php", "composer", "hybrid"),
  reg("vsx", "openvsx"),
  reg("market", "vscode-marketplace"),
  reg("go", "goproxy"),
  reg("apt", "deb"),
  reg("yum", "rpm"),
  reg("js", "npm"),
  reg("crates", "cargo"),
  reg("mvn", "maven"),
  reg("tf", "terraform"),
  reg("tfhost", "terraform", "local", "https://tf.example.test:8443"),
  reg("pypi", "pypi"),
  reg("upstream", "npm", "proxy"),
];

/**
 * The radix tabs render only the active panel, and the select only opens on
 * pointer events jsdom does not lay out: both are replaced by plain markup so
 * every panel is in the DOM and the registry is chosen with `setValue`.
 */
const stubs = {
  Tabs: { template: "<div><slot /></div>" },
  TabsList: { template: "<div><slot /></div>" },
  TabsTrigger: { props: ["value"], template: "<button :data-tab='value'><slot /></button>" },
  TabsContent: { props: ["value"], template: "<section :data-panel='value'><slot /></section>" },
  CodeBlock: { props: ["code"], template: "<pre>{{ code }}</pre>" },
  Select: {
    props: ["modelValue", "options", "id"],
    emits: ["update:modelValue"],
    template: `<select :id="id" :value="modelValue" @change="$emit('update:modelValue', $event.target.value)">
      <option value=""></option>
      <option v-for="o in options" :key="o.value" :value="o.value">{{ o.label }}</option>
    </select>`,
  },
};

const mountWith = (registries = REGISTRIES) =>
  mount(NamespaceUpload, { props: { registries }, global: { stubs } });

async function choose(wrapper: ReturnType<typeof mountWith>, name: string) {
  await wrapper.find("#upload-registry").setValue(name);
}

async function pickFile(wrapper: ReturnType<typeof mountWith>, name = "a.bin") {
  const input = wrapper.find<HTMLInputElement>("#upload-file");
  const file = new File(["bytes"], name);
  Object.defineProperty(input.element, "files", { value: [file], configurable: true });
  await input.trigger("change");
}

const uploadButton = (wrapper: ReturnType<typeof mountWith>) =>
  wrapper.findAll("button").find((b) => ["Upload", "Uploading…"].includes(b.text()))!;

beforeEach(() => {
  authFetchMock.mockReset();
  authFetchMock.mockResolvedValue(new Response("", { status: 201 }));
});

afterEach(() => {
  vi.useRealTimers();
});

describe("NamespaceUpload", () => {
  it("says so when no registry takes uploads", () => {
    const wrapper = mountWith([reg("upstream", "npm", "proxy")]);
    expect(wrapper.text()).toContain("No registries in Local or Hybrid mode are configured.");
    expect(wrapper.find("#upload-registry").exists()).toBe(false);
  });

  it("offers only local and hybrid registries", () => {
    const options = mountWith()
      .findAll("#upload-registry option")
      .map((o) => o.text());
    expect(options).toContain("php");
    expect(options).not.toContain("upstream");
  });

  it("shows a placeholder snippet before a registry is chosen", () => {
    const wrapper = mountWith();
    expect(wrapper.find("pre").text()).toContain(
      "# No CLI instructions available for this registry type.",
    );
    expect(wrapper.find("[data-panel='upload']").exists()).toBe(false);
  });

  it.each([
    ["js", "npm set registry http://localhost:3000/proxy/js\nnpm publish"],
    ["crates", 'index = "sparse+http://localhost:3000/proxy/crates/registry/"'],
    ["mvn", "<url>http://localhost:3000/proxy/mvn/maven2</url>"],
    ["apt", "http://localhost:3000/proxy/apt/deb/pool/stable/main/upload"],
    ["yum", "http://localhost:3000/proxy/yum/rpm/upload"],
    // A subpath registry repeats its name after the host, port kept.
    ["tf", 'source = "localhost:3000/tf/<namespace>/<provider>"'],
    // A host-routed one serves at the root of its own hostname.
    ["tfhost", 'source = "tf.example.test:8443/<namespace>/<provider>"'],
    ["pypi", "# No CLI instructions available for this registry type."],
  ])("writes the CLI snippet for %s", async (name, expected) => {
    const wrapper = mountWith();
    await choose(wrapper, name);
    expect(wrapper.find("pre").text()).toContain(expected);
  });

  it.each([
    ["gems", ".gem"],
    ["php", ".zip"],
    ["vsx", ".vsix"],
    ["market", ".vsix"],
    ["go", ".zip"],
    ["apt", ".deb"],
    ["yum", ".rpm"],
  ])("accepts the %s file type", async (name, accept) => {
    const wrapper = mountWith();
    await choose(wrapper, name);
    expect(wrapper.find("#upload-file").attributes("accept")).toBe(accept);
    expect(wrapper.find("[data-tab='upload']").exists()).toBe(true);
  });

  it("keeps the upload button disabled until a file is picked", async () => {
    const wrapper = mountWith();
    await choose(wrapper, "gems");
    expect(uploadButton(wrapper).attributes("disabled")).toBeDefined();
    await pickFile(wrapper);
    expect(uploadButton(wrapper).attributes("disabled")).toBeUndefined();
  });

  it.each([
    ["gems", {}, "http://localhost:3000/proxy/gems/api/v1/gems", "POST"],
    ["php", {}, "http://localhost:3000/proxy/php/api/upload", "POST"],
    ["yum", {}, "http://localhost:3000/proxy/yum/rpm/upload", "PUT"],
    [
      "vsx",
      { "#upload-ext-id": " ms-python.python ", "#upload-version-ext": "1.0.0" },
      "http://localhost:3000/proxy/vsx/ms-python.python/1.0.0/vsix",
      "PUT",
    ],
    [
      "go",
      { "#upload-module": "github.com/org/repo", "#upload-version-module": "v1.0.0" },
      // A Go module path, `@v` its version marker — not credentials in a URI.
      // nosemgrep: generic.secrets.security.detected-username-and-password-in-uri.detected-username-and-password-in-uri
      "http://localhost:3000/proxy/go/github.com%2Forg%2Frepo/@v/v1.0.0.zip",
      "PUT",
    ],
    [
      "apt",
      { "#upload-distribution": "stable" },
      "http://localhost:3000/proxy/apt/deb/pool/stable/main/upload",
      "PUT",
    ],
  ])("uploads to %s", async (name, fields, url, method) => {
    const wrapper = mountWith();
    await choose(wrapper, name);
    for (const [selector, value] of Object.entries(fields)) {
      await wrapper.find(selector).setValue(value);
    }
    await pickFile(wrapper);
    await uploadButton(wrapper).trigger("click");
    await flushPromises();

    expect(authFetchMock).toHaveBeenCalledTimes(1);
    const [calledUrl, init] = authFetchMock.mock.calls[0];
    expect(calledUrl.endsWith(url.replace("http://localhost:3000", ""))).toBe(true);
    expect(init.method).toBe(method);
    expect(init.headers).toEqual({ "Content-Type": "application/octet-stream" });
    expect(wrapper.text()).toContain("Published successfully.");
    // The form is cleared, so a second click cannot re-send the same file.
    expect(uploadButton(wrapper).attributes("disabled")).toBeDefined();
  });

  it.each([
    ["vsx", "Extension ID and version are required"],
    ["market", "Extension ID and version are required"],
    ["go", "Module path and version are required"],
  ])("refuses a %s upload without its coordinates", async (name, message) => {
    const wrapper = mountWith();
    await choose(wrapper, name);
    await pickFile(wrapper);
    await uploadButton(wrapper).trigger("click");
    await flushPromises();
    expect(authFetchMock).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain(message);
  });

  it("refuses a deb upload whose component was cleared", async () => {
    const wrapper = mountWith();
    await choose(wrapper, "apt");
    await wrapper.find("#upload-distribution").setValue("stable");
    await wrapper.find("#upload-component").setValue("  ");
    await pickFile(wrapper);
    await uploadButton(wrapper).trigger("click");
    await flushPromises();
    expect(authFetchMock).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain("Distribution and component are required");
  });

  it("shows what the server answered when it refuses", async () => {
    authFetchMock.mockResolvedValue(new Response("version already exists", { status: 409 }));
    const wrapper = mountWith();
    await choose(wrapper, "gems");
    await pickFile(wrapper);
    await uploadButton(wrapper).trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("version already exists");
    expect(wrapper.text()).not.toContain("Published successfully.");
  });

  it("says Uploading… while the request is in flight, and clears the success line later", async () => {
    vi.useFakeTimers();
    let answer!: (r: Response) => void;
    authFetchMock.mockReturnValue(new Promise<Response>((resolve) => (answer = resolve)));
    const wrapper = mountWith();
    await choose(wrapper, "gems");
    await pickFile(wrapper);
    await uploadButton(wrapper).trigger("click");
    await flushPromises();
    expect(uploadButton(wrapper).text()).toBe("Uploading…");

    answer(new Response("", { status: 201 }));
    await flushPromises();
    expect(wrapper.text()).toContain("Published successfully.");
    vi.advanceTimersByTime(4000);
    await flushPromises();
    expect(wrapper.text()).not.toContain("Published successfully.");
  });
});
