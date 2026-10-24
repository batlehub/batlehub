import { mount, flushPromises } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { nextTick } from "vue";

import DisplaySettings from "./DisplaySettings.vue";
import { setLocalePreference } from "@/i18n";

/**
 * Theme and language are three-state *preferences* (RFC 0003 R12, §4.6), drawn
 * as one choice set each with every answer on screen — not two buttons that
 * cycled with the current state only in an aria-label.
 */
async function open() {
  const wrapper = mount(DisplaySettings, { attachTo: document.body });
  await wrapper.get("button").trigger("click");
  await flushPromises();
  return wrapper;
}

const radio = (name: string, value: string) =>
  document.querySelector<HTMLInputElement>(`input[name="${name}"][value="${value}"]`)!;

async function choose(name: string, value: string) {
  const input = radio(name, value);
  input.checked = true;
  input.dispatchEvent(new Event("change", { bubbles: true }));
  await nextTick();
}

describe("DisplaySettings", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
  });
  afterEach(() => {
    setLocalePreference("en");
    document.body.innerHTML = "";
  });

  it("names itself, and shows every answer of both choices at once", async () => {
    const wrapper = await open();
    expect(wrapper.get("button").attributes("aria-label")).toBe("Display");
    for (const v of ["auto", "light", "dark"]) expect(radio("display-theme", v)).toBeTruthy();
    for (const v of ["system", "en", "fr"]) expect(radio("display-locale", v)).toBeTruthy();
  });

  it("starts on the system theme rather than a hardcoded one", async () => {
    await open();
    expect(radio("display-theme", "auto").checked).toBe(true);
  });

  /** The token layer keys off data-theme; a class would style nothing. */
  it("writes the chosen rendition to data-theme on <html>", async () => {
    await open();
    await choose("display-theme", "light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    await choose("display-theme", "dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });

  /** "System" is storable — the thing a two-way switch could not express. */
  it("stores the preference, not the resolved value", async () => {
    await open();
    await choose("display-theme", "light");
    expect(localStorage.getItem("batlehub.theme")).toBe("light");
    await choose("display-theme", "auto");
    expect(localStorage.getItem("batlehub.theme")).toBe("auto");
  });

  it("switches the language, and keeps System as a choice of its own", async () => {
    await open();
    await choose("display-locale", "fr");
    expect(radio("display-locale", "fr").checked).toBe(true);
    await choose("display-locale", "system");
    expect(radio("display-locale", "system").checked).toBe(true);
  });
});
