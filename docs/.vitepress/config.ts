import { fileURLToPath } from "node:url";
import { defineConfig } from "vitepress";
import { withMermaid } from "vitepress-plugin-mermaid";

// The banner, the table on /rfc/ and the sidebar in `nav/en.ts` all quote the
// same header rows, so the parser lives in one place — `build/rfc-meta.mjs`,
// shared with `task rfc:index` and `task rfc:status`.
import { parseFilename, rfcStatus } from "../build/rfc-meta.mjs";

// One module per locale, holding that locale's navbar and sidebars — every
// navigation label the site has. They are imported rather than written inline
// because the two gates that read them (`check-audience.mjs`,
// `check-links.mjs`) import them too, and a list two things read from one place
// cannot disagree with itself.
import { nav as navEn, sidebar as sidebarEn } from "./nav/en.ts";
import { nav as navFr, sidebar as sidebarFr } from "./nav/fr.ts";

// `withMermaid` turns every ```mermaid fence into a rendered diagram. Without
// it the fences shipped as syntax-highlighted source: valid, readable, and not
// what a reader of an architecture section is looking for. It only wraps the
// config — `defineConfig` still types everything below.
//
// Diagram *syntax* is not checked by the build (the plugin renders on the
// client), which is why `task docs:mermaid` parses every fence separately.
const config = withMermaid(defineConfig({
  appearance: "dark",
  title: "BatleHub",
  description:
    "Your package hub. Proxy, cache, and host npm, Cargo, Maven, PyPI, NuGet, Go, RubyGems, Terraform, and more.",
  cleanUrls: false,
  base: process.env.BASE_URL || "/",

  // Merging the two documentation trees is not the same as publishing both
  // (RFC 0005 §6.7). Three classes stay in the repo and out of the build:
  // generated artifacts (`i18n-review-fr.md` is output from
  // `task ui:i18n:review`, not a document), point-in-time security findings,
  // and forms — the RFC template is something you copy, not something you read.
  //
  // The rule is worth stating plainly, because the RFCs next door *do* publish
  // and they are candid about this project's own past defects: design history
  // publishes, security findings do not. Visible rigour about one's own
  // mistakes reads as competence; a dated vulnerability survey reads as a map.
  srcExclude: ["internal/**"],

  // Generated from each RFC's own `Status` row — see `rfcStatus` above and the
  // banner in `theme/RfcStatus.vue`. An unparseable status fails the build
  // rather than rendering an unlabelled page.
  //
  // Only a numbered RFC carries one. `rfc/index.md` and `rfc/plan.md` are
  // about the RFCs — a listing and a build plan — and neither has a status of
  // its own to quote, so they are recognised by the same filename rule
  // `task rfc:index` uses to decide what is an RFC at all.
  transformPageData(pageData, ctx) {
    const inRfc = pageData.filePath.startsWith("rfc/");
    if (inRfc && parseFilename(pageData.filePath.slice("rfc/".length))) {
      pageData.frontmatter.rfcStatus = rfcStatus(
        ctx.siteConfig.srcDir,
        pageData.filePath,
      );
    }
  },
  vite: {
    server: {
      allowedHosts: true,
      host: true,
    },
    plugins: [
      // Drop the default theme's Inter. See theme/no-inter.css for why.
      //
      // A `resolve.alias` entry cannot do this: aliases match the import
      // specifier as written, and the default theme imports its own stylesheet
      // as the relative `./styles/fonts.css`. Matching that string alone would
      // catch any file of that name in any package. Resolving by importer is
      // what makes the redirect specific to VitePress's own theme.
      {
        name: "batlehub:no-inter",
        enforce: "pre" as const,
        resolveId(source: string, importer?: string) {
          if (
            source.endsWith("styles/fonts.css") &&
            importer?.includes("theme-default")
          ) {
            return fileURLToPath(
              new URL("./theme/no-inter.css", import.meta.url),
            );
          }
          return null;
        },
      },
    ],
  },
  head: [
    [
      "link",
      {
        rel: "icon",
        type: "image/svg+xml",
        href: (process.env.BASE_URL || "/") + "logo.svg",
      },
    ],

    // The two specimen faces, self-hosted (RFC 0005 §6.4). Preloaded because
    // both are in the first paint: headings and the wordmark are Silkscreen and
    // every other string on the page is JetBrains Mono. Silkscreen 400 is not
    // preloaded — every Silkscreen rule in the theme asks for 700.
    //
    // `@font-face` declares these in `theme/vp-bridge.css` with root-absolute
    // paths; the preload hrefs carry BASE_URL for the same reason logo.svg does,
    // since the site publishes under a prefix.
    [
      "link",
      {
        rel: "preload",
        as: "font",
        type: "font/woff2",
        crossorigin: "",
        href: (process.env.BASE_URL || "/") + "fonts/silkscreen-700.woff2",
      },
    ],
    [
      "link",
      {
        rel: "preload",
        as: "font",
        type: "font/woff2",
        crossorigin: "",
        href: (process.env.BASE_URL || "/") + "fonts/jetbrainsmono-latin.woff2",
      },
    ],

    // The rendition sync. `tokens.css` authors light under
    // `:root[data-theme="light"]`; VitePress carries its resolved appearance as
    // a `.dark` class on the same element. This mirrors one onto the other so
    // there is a single stored preference and a single resolution — The
    // Stored-Preference Rule wants one mechanism, and VitePress already has it
    // (it stores `system|light|dark` and resolves before first paint).
    //
    // The initial call plus the observer covers both injection orders: if
    // VitePress's appearance script has already run, the first call is right; if
    // it has not, the observer fires when it does. Both happen in the head, and
    // observer callbacks are microtasks, so neither case reaches a paint with
    // the wrong ground. The filter is `class`, so writing `data-theme` cannot
    // re-trigger it.
    [
      "script",
      {},
      `(()=>{const e=document.documentElement,s=()=>e.setAttribute("data-theme",e.classList.contains("dark")?"dark":"light");s();new MutationObserver(s).observe(e,{attributes:true,attributeFilter:["class"]})})()`,
    ],
    ["meta", { name: "theme-color", content: "#dc2626" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:site_name", content: "BatleHub" }],
    ["meta", { property: "og:title", content: "BatleHub" }],
    [
      "meta",
      {
        property: "og:description",
        content:
          "Your package hub. Proxy, cache, and host npm, Cargo, Maven, PyPI, NuGet, Go, RubyGems, Terraform, and more.",
      },
    ],
    [
      "meta",
      {
        property: "og:image",
        content: (process.env.BASE_URL || "/") + "logo.svg",
      },
    ],
    ["meta", { name: "twitter:card", content: "summary" }],
    ["meta", { name: "twitter:title", content: "BatleHub" }],
    [
      "meta",
      {
        name: "twitter:description",
        content:
          "Your package hub. Proxy, cache, and host npm, Cargo, Maven, PyPI, NuGet, Go, RubyGems, Terraform, and more.",
      },
    ],
    [
      "meta",
      {
        name: "twitter:image",
        content: (process.env.BASE_URL || "/") + "logo.svg",
      },
    ],
  ],

  markdown: {
    // Syntax highlighting is the one colour system on this site the design
    // tokens do not decide — a highlighter's palette is a mapping from grammar
    // to hue, and DESIGN.md's four colours cannot express one. What it *can*
    // decide is the floor: every token has to clear AA against the ground the
    // code block actually sits on, which is `--ground-sunk` in both renditions.
    //
    // VitePress's defaults do not. Measured by `docs:design:rendered`:
    // `github-dark`'s comment token (#6a737d) lands at 4.35:1 on near-black and
    // `github-light`'s string token (#22863a) at 4.49:1 on paper — the second
    // under the bar by a hundredth, which is exactly the kind of miss nothing
    // but a measurement finds. GitHub's Primer defaults fix the dark ground and
    // still leave the light comment at 4.41:1, because this world's paper is
    // `oklch(0.99 0.004 18)` rather than #ffffff and a palette tuned against
    // pure white does not survive the move. `light-plus`/`dark-plus` clear both.
    theme: { light: "github-light-high-contrast", dark: "github-dark-high-contrast" },
  },

  // Shared by both locales. Everything that carries a translatable string —
  // the navbar, the sidebars, the footer, the theme's own chrome — lives under
  // `locales` below instead: VitePress deep-merges a locale's `themeConfig`
  // over this one, and that merge is what keeps the shared half from being
  // maintained twice.
  themeConfig: {
    logo: "/logo.svg",
    siteTitle: "BatleHub.",

    socialLinks: [
      { icon: "git", link: "https://git.batleforc.fr/batleforc/batlehub" },
      { icon: "github", link: "https://github.com/batlehub/batlehub" },
    ],

    // The local provider indexes each locale separately on its own — a French
    // search that returned English pages would return pages the reader came
    // here not to read — so the only thing to declare is the interface around
    // the results, which is otherwise served in English on a French page.
    search: {
      provider: "local",
      options: {
        locales: {
          fr: {
            translations: {
              button: {
                buttonText: "Rechercher",
                buttonAriaLabel: "Rechercher",
              },
              modal: {
                displayDetails: "Afficher le détail",
                resetButtonTitle: "Effacer la recherche",
                backButtonTitle: "Fermer la recherche",
                noResultsText: "Aucun résultat pour",
                footer: {
                  selectText: "pour sélectionner",
                  selectKeyAriaLabel: "entrée",
                  navigateText: "pour naviguer",
                  navigateUpKeyAriaLabel: "flèche haut",
                  navigateDownKeyAriaLabel: "flèche bas",
                  closeText: "pour fermer",
                  closeKeyAriaLabel: "échap",
                },
              },
            },
          },
        },
      },
    },
  },

  // Two trees, one build. `docs/internal/glossaire-fr.md` holds the wording
  // decisions behind the second one; `task docs:i18n:status` says how much of
  // it exists.
  //
  // English stays at the root, so no published URL moves and there is no
  // redirect table to maintain; French is served under `/fr/`. VitePress does
  // not fall back: a page that exists in English and not in French is a 404
  // under `/fr/`, never a silent return to the English text. That is why the
  // French navigation links some destinations at their English URL rather than
  // dropping them, and why `check-links.mjs` has a rule for exactly that.
  locales: {
    root: {
      label: "English",
      lang: "en-US",
      themeConfig: {
        nav: navEn,
        sidebar: sidebarEn,
        footer: {
          message:
            "Released under the Apache 2.0 License. Made with ❤️ and too much ☕.",
          copyright: "Copyright © 2026 Batleforc",
        },
      },
    },

    fr: {
      label: "Français",
      lang: "fr-FR",
      link: "/fr/",
      description:
        "Votre hub de paquets. Proxy, cache et hébergement pour npm, Cargo, Maven, PyPI, NuGet, Go, RubyGems, Terraform et bien d'autres.",
      themeConfig: {
        nav: navFr,
        sidebar: sidebarFr,
        outline: { label: "Sur cette page" },
        docFooter: { prev: "Page précédente", next: "Page suivante" },
        darkModeSwitchLabel: "Apparence",
        lightModeSwitchTitle: "Passer au thème clair",
        darkModeSwitchTitle: "Passer au thème sombre",
        sidebarMenuLabel: "Menu",
        returnToTopLabel: "Retour en haut",
        langMenuLabel: "Changer de langue",
        skipToContentLabel: "Aller au contenu",
        notFound: {
          title: "PAGE INTROUVABLE",
          quote:
            "Cette page n'existe pas en français. La traduction se fait par vagues, et le sélecteur de langue en haut mène au même chemin en anglais.",
          linkLabel: "Retour à l'accueil",
          linkText: "Retour à l'accueil",
        },
        footer: {
          message:
            "Publié sous licence Apache 2.0. Fait avec ❤️ et beaucoup trop de ☕.",
          copyright: "Copyright © 2026 Batleforc",
        },
      },
    },
  },
}));

// `withMermaid` pre-declares mermaid's CommonJS dependencies for the dev
// server's dependency optimiser, and it does so with bare names —
// `optimizeDeps.include = ["@braintree/sanitize-url", "dayjs", "debug",
// "cytoscape", "cytoscape-cose-bilkent"]`. Those names resolve from the project
// root, and under pnpm's isolated `node_modules` none of them are there: they
// are mermaid's dependencies, not the docs site's, so they only exist under
// `.pnpm/…/node_modules/mermaid/node_modules`. Vite skips every entry it cannot
// resolve, so nothing gets pre-bundled.
//
// What that costs is not a warning, it is a broken page. `mermaid` itself is
// never scanned — the plugin injects its import from a `transform` hook with
// `enforce: "post"`, after the optimiser's scan — so it is served straight from
// source, and its `import dayjs from "dayjs"` lands on dayjs's raw UMD file.
// That file has no ESM exports, and the browser fails the module with
// "doesn't provide an export named: 'default'".
//
// Naming `mermaid` is the fix, and the reason the nested `mermaid > …` entries
// are here too rather than deleted: esbuild bundles mermaid *and* its CJS
// dependencies into one pre-bundled ESM chunk, so the interop happens at build
// time and no raw CJS file is ever requested. The `mermaid > x` form is Vite's
// own syntax for "resolve x the way mermaid would", which is exactly the step
// pnpm's layout breaks.
//
// `debug` is dropped rather than rewritten: mermaid 11 no longer depends on it,
// and an unresolvable entry is exactly the failure being fixed here.
config.vite!.optimizeDeps!.include = [
  "mermaid",
  "mermaid > @braintree/sanitize-url",
  "mermaid > dayjs",
  "mermaid > cytoscape",
  "mermaid > cytoscape-cose-bilkent",
];

export default config;
