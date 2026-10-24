<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { explorePackageReadme } from "@/client/sdk.gen";
import type { ReadmeResponse } from "@/client/types.gen";
import { Button } from "@/components/ui/button";
import { highlightInto, useShiki } from "@/composables/useShiki";

/**
 * A package version's own account of itself.
 *
 * **The only component in the console that renders server-supplied HTML.** The
 * text is attacker-authored by construction — anyone who can publish to a
 * proxied upstream can write it — and it is displayed on the console's own
 * origin to a session that is frequently an administrator's.
 *
 * Nothing is sanitised here, and that is the design rather than an omission
 * (RFC 0007 §2.6): the boundary is server-side, in `crates/core/src/services/
 * readme/sanitize.rs`, where an allow-list over an `html5ever` parse has unit
 * tests, a fuzz target and `cargo deny` over its dependencies. A markdown
 * renderer plus a DOM sanitiser in this bundle would put that boundary where
 * the fuzz suite cannot reach it, and would have to be reimplemented for the
 * CLI and every other client.
 *
 * `ReadmePanel.test.ts` asserts that no other component grows a `v-html`, so
 * the boundary cannot quietly move.
 */
const props = defineProps<{
  registry: string;
  name: string;
  /** The version the page has selected. `null` asks for the newest that has one. */
  version: string | null;
}>();

const { t } = useI18n();

const readme = ref<ReadmeResponse | null>(null);
const loading = ref(false);
/** The `code` slug from the error body — `readme.none-stored` and friends. */
const absence = ref<string | null>(null);
const failure = ref<string | null>(null);

/**
 * Which `load()` the panel is currently showing.
 *
 * The version rows on the detail page are clickable, so two clicks in quick
 * succession issue two reads; without this the slower one lands last and the
 * panel shows version A's prose — with A's version in the card title — while
 * the table marks B as selected. Nothing catches it downstream: the response is
 * internally consistent, just for the wrong ask, so `fallbackNotice` stays
 * silent. Same `seq` guard the catalogue and `fetchDetail` already use.
 */
let readmeSeq = 0;

async function load() {
  const seq = ++readmeSeq;
  loading.value = true;
  readme.value = null;
  absence.value = null;
  failure.value = null;
  try {
    const { data, error } = await explorePackageReadme({
      path: { registry: props.registry, name: props.name },
      // `both`, because the panel now offers the source as well as the
      // rendering. The default is `html`, which returns `source_text: null` —
      // so the toggle had nothing to switch to, and the reason was a query
      // parameter rather than anything missing from the store.
      query: { format: "both", ...(props.version ? { version: props.version } : {}) },
    });
    if (seq !== readmeSeq) return;
    if (error) {
      // A `404`/`403` here is information, not a failure: the panel renders a
      // statement for each shape rather than an error banner (§4.4).
      const body = error as { code?: string; message?: string };
      absence.value = body.code ?? "readme.none-stored";
      failure.value = body.message ?? null;
      return;
    }
    readme.value = data as ReadmeResponse;
  } catch {
    if (seq !== readmeSeq) return;
    absence.value = "readme.unreachable";
  } finally {
    // Only the newest read owns the spinner: an older one finishing later must
    // not clear it while the current read is still in flight.
    if (seq === readmeSeq) loading.value = false;
  }
}

watch(() => [props.registry, props.name, props.version], load, {
  immediate: true,
});

/**
 * The sentence the panel leads with when the text on screen is not the version
 * the reader selected.
 *
 * Spelled out rather than a badge: prose that belongs to different code is the
 * one thing a README panel must never present silently (§4.4).
 */
const fallbackNotice = computed(() => {
  if (!readme.value?.is_fallback || !readme.value.requested_version) return null;
  return t("readmePanel.fallbackFrom", {
    shown: readme.value.version,
    requested: readme.value.requested_version,
  });
});

/**
 * npm's document-root README describes the package, not this version. Saying so
 * is the difference between a fact and a guess presented as one.
 */
const packageLevelNotice = computed(() =>
  readme.value?.package_level ? t("readmePanel.packageLevel") : null,
);

const truncationNotice = computed(() =>
  readme.value?.truncated ? t("readmePanel.truncated") : null,
);

/**
 * A derived answer is read from the cached upstream document rather than from a
 * durable record — which is what makes it possible at all for a version this
 * instance holds no bytes for.
 */
const derivedNotice = computed(() =>
  readme.value && !readme.value.stored ? t("readmePanel.notHeldHere") : null,
);

// ── Rendered, or the source it was rendered from ─────────────────────────────
//
// A README is a document *and* a file, and the two answer different questions.
// The rendered view answers "what does this package say"; the source answers
// "what exactly is stored here" — which is the one a reader needs when the
// rendering surprises them, when an image was stripped, or when they are about
// to publish something similar and want the markup that produced it.
//
// The endpoint already sends both: `rendered_html` and `source_text` (RFC 0007
// §4.4), the second absent only when the source *was* HTML. So this is a control
// over data the page already has, not a second request.
type ReadmeView = "rendered" | "source";
const view = ref<ReadmeView>("rendered");

/** Offered only when there are two things to switch between. */
const canShowSource = computed(() => !!readme.value?.source_text && !!readme.value.rendered_html);

/* A new package, or a new version, is a new document: it opens rendered. */
watch(
  () => [props.registry, props.name, props.version],
  () => {
    view.value = "rendered";
  },
);

const bodyEl = ref<HTMLElement | null>(null);
const sourceEl = ref<HTMLElement | null>(null);

/* `ready` is watched, not awaited: the highlighter's chunk may land after the
   README does, and a block that stays plain because the page rendered first is
   the defect this dependency exists to avoid. */
const { ready: shikiReady } = useShiki();

/**
 * Highlight the fenced blocks of the rendered document.
 *
 * The language comes from the class the server's renderer writes
 * (`<code class="language-rust">`), and an unknown one paints nothing and keeps
 * the plain block — a README can fence `mermaid` or nothing at all.
 *
 * Nothing here writes HTML. `highlightInto` builds spans and sets their text
 * through `textContent`, so package-authored bytes never re-enter the page as
 * markup, and the component keeps the single `v-html` its own test counts.
 */
async function paintRendered() {
  const root = bodyEl.value;
  if (!root) return;
  // Each block paints into its own element, so they go in parallel; a grammar
  // two blocks share may load twice, which Shiki's `loadLanguage` tolerates.
  const blocks = [...root.querySelectorAll("pre > code")] as HTMLElement[];
  await Promise.all(
    blocks.map(async (el) => {
      if (el.dataset.highlighted === "true") return;
      const lang = /language-([\w#+.-]+)/.exec(el.className)?.[1] ?? "";
      const text = el.textContent ?? "";
      if (!lang || !text.trim()) return;
      if (await highlightInto(el, text, lang)) el.dataset.highlighted = "true";
    }),
  );
}

/** The source view is itself a code block, so it is highlighted as one. */
async function paintSource() {
  const el = sourceEl.value;
  const text = readme.value?.source_text;
  if (!el || !text) return;
  // `format` is what the source *is* — `markdown` | `rst` | `plain`. Handing
  // Shiki the wrong grammar is worse than handing it none.
  await highlightInto(el, text, readme.value?.format ?? "");
}

watch([readme, view, shikiReady], async () => {
  await nextTick();
  if (view.value === "rendered") await paintRendered();
  else await paintSource();
});

const absenceMessage = computed(() => {
  switch (absence.value) {
    case "readme.unsupported-type":
      return t("readmePanel.noneForThisRegistryType");
    case "readme.blocked":
      return t("readmePanel.blocked");
    case "readme.unreachable":
      return t("readmePanel.unreachable");
    case "readme.none-stored":
      return t("readmePanel.noneStored");
    default:
      return null;
  }
});
</script>

<template>
  <!-- A region separated by a full-width hairline, not a card. The world has
       none, and this was one of three stacked on the package page.

       The icon went with it: a crimson glyph beside a heading spends the one
       synthetic colour in the system on decoration, and crimson has four jobs,
       none of which is "there is a README here". The heading says that. -->
  <section class="border-t border-rule-soft pt-6" aria-labelledby="readme-heading">
    <div class="flex flex-wrap items-center justify-between gap-3 pb-3">
      <h2 id="readme-heading" class="flex items-center gap-2 font-mono text-base text-foreground">
        {{ t("readmePanel.title") }}
        <span v-if="readme" class="font-mono text-xs text-muted-foreground">
          {{ readme.version }}
        </span>
      </h2>
      <!-- One control, labelled with the view it switches *to* rather than
           the one you are in: a button that says where you already are makes
           a reader test it to find out. No `aria-pressed` on top of that — the
           label already inverts, and the two together announce as a double
           negative. -->
      <Button
        v-if="canShowSource"
        variant="outline"
        size="sm"
        class="px-2 text-xs"
        @click="view = view === 'rendered' ? 'source' : 'rendered'"
      >
        {{ view === "rendered" ? t("readmePanel.showSource") : t("readmePanel.showRendered") }}
      </Button>
    </div>
    <div>
      <p v-if="loading" class="text-sm text-muted-foreground">
        {{ t("common.loading") }}
      </p>

      <template v-else-if="readme">
        <p v-if="fallbackNotice" class="text-sm text-muted-foreground mb-3 max-w-[72ch]">
          {{ fallbackNotice }}
        </p>
        <p v-if="packageLevelNotice" class="text-sm text-muted-foreground mb-3 max-w-[72ch]">
          {{ packageLevelNotice }}
        </p>
        <p v-if="derivedNotice" class="text-sm text-muted-foreground mb-3 max-w-[72ch]">
          {{ derivedNotice }}
        </p>
        <!--
          The one deliberate `v-html` in the console. The rule is right in
          general and wrong here: the HTML is produced and sanitised
          server-side, by an allow-list over an `html5ever` parse with a fuzz
          target and `cargo deny` over it (RFC 0007 §2.6). Re-sanitising in the
          bundle would put the boundary where the fuzz suite cannot reach it.
          `ReadmePanel.test.ts` asserts no third component grows one.
        -->
        <!-- A disable/enable *pair*, not `disable-next-line`: the rule reports at
             the `v-html` attribute, four lines into the element, so the
             single-line form suppressed nothing and the warning kept surfacing
             as a code-scanning alert on every PR. This form does not care where
             in the element the attribute sits. -->
        <!-- eslint-disable vue/no-v-html -->
        <div
          v-if="view === 'rendered'"
          ref="bodyEl"
          class="readme-body"
          v-html="readme.rendered_html ?? ''"
        ></div>
        <!-- eslint-enable vue/no-v-html -->
        <!-- The source as **text**, through interpolation. It is the same bytes
             the rendered view came from, and the whole reason a reader opens
             this is to see them exactly — so this is the one place they must not
             be parsed. Shiki paints over it afterwards without changing a
             character. -->
        <pre v-else ref="sourceEl" class="readme-source">{{ readme.source_text }}</pre>
        <p v-if="truncationNotice" class="text-xs text-muted-foreground mt-3 max-w-[72ch]">
          {{ truncationNotice }}
        </p>
      </template>

      <template v-else>
        <p class="text-sm text-muted-foreground">
          {{ absenceMessage ?? failure ?? t("readmePanel.noneStored") }}
        </p>
      </template>
    </div>
  </section>
</template>

<style scoped>
/* Typography for the sanitised document, scoped so a README cannot restyle the
   console around it. The sanitiser drops `style` entirely — both the element
   and the attribute — so nothing here can be overridden from the text.
 *
 * Every value is a design token. A README is somebody else's document rendered
 * inside ours, which makes it exactly the place where inventing a type ramp
 * would show: `--t-head`/`--t-sub`/`--t-body`/`--t-meta` are the steps this
 * console has, and `--radius` is zero because "the world has no rounded
 * corner" (tokens.css). */
.readme-body :deep(h1),
.readme-body :deep(h2),
.readme-body :deep(h3) {
  font-weight: 600;
  margin: var(--s3) 0 var(--s2);
  line-height: 1.3;
}
.readme-body :deep(h1) {
  font-size: var(--t-head);
}
.readme-body :deep(h2) {
  font-size: var(--t-sub);
}
.readme-body :deep(h3) {
  font-size: var(--t-row);
}
.readme-body :deep(p),
.readme-body :deep(ul),
.readme-body :deep(ol) {
  margin: var(--s2) 0;
  font-size: var(--t-body);
  line-height: 1.6;
  /* The panel's own measure, on the running text and nothing else.

     `PackageDetailPage` used to cap the whole page at `max-w-4xl`, which kept
     this prose readable by accident while squeezing the versions table into
     846px. With the page full-bleed, a README on a 1920 window would otherwise
     set 230-character lines. 72ch is the measure the specimen caption already
     uses, so the console keeps one measure vocabulary rather than growing a
     third number.

     On `p`/`ul`/`ol` rather than on `.readme-body`: capping the container
     capped everything inside it, and a README's code blocks, tables and images
     are not prose — measured on `react-smooth`, whose own table came out at
     562px, narrower than it had been before the page went full width. They now
     take the card, and `pre`/`table` still scroll inside themselves. */
  max-width: 72ch;
}
.readme-body :deep(ul),
.readme-body :deep(ol) {
  padding-left: var(--s4);
  /* Tailwind's preflight removes list markers; a README's lists are prose and
     need them back. */
  list-style: revert;
}
.readme-body :deep(code) {
  font-family: var(--face-text);
  font-size: var(--t-meta);
  background: var(--ground-raised);
  padding: 0 var(--s1);
}
.readme-body :deep(pre) {
  background: var(--ground-raised);
  padding: var(--s3);
  overflow-x: auto;
  font-size: var(--t-meta);
}
.readme-body :deep(pre code) {
  background: none;
  padding: 0;
}
/* Shiki paints each token's colour onto a custom property; the same pair the
   console's own `CodeBlock` uses, so a README block and a Setup Guide snippet
   are lit by one theme rather than two. A block Shiki did not recognise sets
   neither property and keeps the inherited ink.

   `:deep()` on **both** branches. The source view's spans are built
   imperatively by `highlightInto` (`document.createElement("span")`), so they
   carry no `data-v-` attribute: scoped `.readme-source span` compiles to
   `.readme-source span[data-v-x]` and matches none of them, and the whole
   "Show source" block rendered in the inherited foreground colour with every
   `--shiki-light` set and unused. */
.readme-body :deep(pre code span),
.readme-source :deep(span) {
  color: var(--shiki-light);
}
[data-theme="dark"] .readme-body :deep(pre code span),
[data-theme="dark"] .readme-source :deep(span) {
  color: var(--shiki-dark);
}
.readme-source {
  background: var(--ground-raised);
  padding: var(--s3);
  overflow-x: auto;
  font-family: var(--face-text);
  font-size: var(--t-meta);
  line-height: 1.6;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  margin: 0;
}
.readme-body :deep(table) {
  border-collapse: collapse;
  font-size: var(--t-meta);
  /* A wide table scrolls inside its own box rather than widening the page. */
  display: block;
  overflow-x: auto;
}
.readme-body :deep(th),
.readme-body :deep(td) {
  border: 1px solid var(--rule-soft);
  padding: var(--s1) var(--s2);
}
.readme-body :deep(a) {
  text-decoration: underline;
  text-underline-offset: 2px;
}
.readme-body :deep(blockquote) {
  border-left: 2px solid var(--rule-soft);
  padding-left: var(--s3);
  color: var(--ink-dim);
}
/* `align` survives the sanitiser on block elements (see `ALIGNABLE_TAGS` in
   `crates/core/src/services/readme/sanitize.rs`), because a centred badge row
   written as `<p align="center">` is the commonest raw HTML in a README and
   dropping it renders the document differently from how its author wrote it.

   Mapped here rather than left to the browser: `align` is an obsolete
   presentational attribute that the HTML rendering spec still defines, but
   "still defined" is a weaker guarantee than a rule of our own, and this is one
   declaration. `text-align` is the whole of the effect — it centres inline
   content and cannot position or layer anything, which is why the attribute is
   allowed at all where `style` is not. */
.readme-body :deep([align="center"]) {
  text-align: center;
}
.readme-body :deep([align="right"]) {
  text-align: right;
}
.readme-body :deep([align="left"]) {
  text-align: left;
}
.readme-body :deep([align="justify"]) {
  text-align: justify;
}

/* The chip that replaces a stripped image: the reader sees that an image was
   there and where it pointed, which is the whole of what a badge row says. */
.readme-body :deep(.readme-stripped-image) {
  display: inline-block;
  border: 1px dashed var(--rule-soft);
  padding: 0 var(--s1);
  font-size: var(--t-meta);
  color: var(--ink-dim);
}

/* Under `remote_images = "proxy"` the panel receives real image elements,
   pointing at this server. It does not know or care which policy produced them —
   that is the point of doing the rewriting server-side (RFC 0007-bis §6.4).

   (Spelled out rather than written as the literal tag: `impeccable detect`
   scans source text, and a bare tag with no `src` in a comment reads to it as a
   shipped broken image — see .github/workflows/front-design.yaml.)

   The styling is written for the failure case, because the endpoint answers 404
   for every image it could not get: a dead URL, a type that is not an image, one
   over the cap, an SVG the sanitiser refused. A broken-image icon would be a
   worse answer than the one `strip` gives, so a broken image is styled to read
   like the chip — dashed, dim, and showing its alt text. */
.readme-body :deep(img) {
  max-width: 100%;
  height: auto;
  vertical-align: middle;
  font-size: var(--t-meta);
  color: var(--ink-dim);
}
.readme-body :deep(img:not([src])) {
  display: none;
}
</style>
