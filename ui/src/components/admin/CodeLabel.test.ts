import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import fr from "@/locales/fr.json";
import CodeLabel from "./CodeLabel.vue";
import {
  AUDIT_ACTION_LABEL_KEYS,
  FLAG_EFFECT_LABEL_KEYS,
  REASON_LABEL_KEYS,
} from "@/config/codeLabels";

function mountIn(locale: "en" | "fr", code: string, labels: Record<string, string>) {
  const i18n = createI18n({ legacy: false, locale, messages: { en, fr } });
  return mount(CodeLabel, { props: { code, labels }, global: { plugins: [i18n] } });
}

describe("CodeLabel (RFC 0050 §4.2)", () => {
  it("draws the label with the raw code beside it, in both locales", () => {
    const en = mountIn("en", "SOC_VERDICT", REASON_LABEL_KEYS);
    expect(en.findAll("span > span, span > code").map((e) => e.text())).toEqual([
      "SOC verdict",
      "SOC_VERDICT",
    ]);
    expect(en.get("code").classes()).toContain("font-mono");
    expect(mountIn("fr", "SOC_VERDICT", REASON_LABEL_KEYS).text()).toContain("Verdict du SOC");
  });

  it("reads an effect as its consequence", () => {
    expect(mountIn("en", "hard_block", FLAG_EFFECT_LABEL_KEYS).text()).toContain(
      "blocks downloads",
    );
  });

  it("draws an unknown code raw, never a missing key", () => {
    const w = mountIn("en", "NEW_CODE", REASON_LABEL_KEYS);
    expect(w.text()).toBe("NEW_CODE");
  });

  /**
   * The audit-log JSON spells actions as serde does (`rename_all = "lowercase"`,
   * the generated `AccessAction` union), the timeline as `as_str` does; the
   * table was keyed by the second alone and labelled 10 of 46.
   */
  it("labels every audit action under both wire spellings", () => {
    const squashed = [
      "download",
      "viewmetadata",
      "block",
      "unblock",
      "delete",
      "addowner",
      "removeowner",
      "setvisibility",
      "blockuser",
      "unblockuser",
      "blockip",
      "unblockip",
      "auditpurge",
      "yank",
      "unyank",
      "deprecate",
      "undeprecate",
      "unlist",
      "relist",
      "addbetamember",
      "removebetamember",
      "claimnamespace",
      "releasenamespace",
      "resetquota",
      "tombstonecompact",
      "grantwrite",
      "grantrevoke",
      "setretentionpin",
      "retentionreclaim",
      "retentionrun",
      "retentiondryrun",
      "cacheevict",
      "cacheclear",
      "cacheevictrun",
      "cacheevictdryrun",
      "cachecoherencerun",
      "cachecoherencedryrun",
      "signin",
      "signinfailed",
      "tokencreate",
      "tokenrevoke",
      "tokennewsource",
      "credentialrejected",
      "gdprerase",
      "gdprexport",
      "auditlifecyclerun",
    ];
    expect(squashed).toHaveLength(46);
    expect(squashed.filter((a) => !AUDIT_ACTION_LABEL_KEYS[a])).toEqual([]);
    expect(mountIn("en", "signinfailed", AUDIT_ACTION_LABEL_KEYS).text()).toContain(
      en.auditAction.sign_in_failed,
    );
    expect(AUDIT_ACTION_LABEL_KEYS.sign_in_failed).toBe("auditAction.sign_in_failed");
  });
});
