import { defineConfig } from "vitest/config";
import { loadEnv, type Plugin } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

import { buildCsp, resolveLivePort } from "./build/csp.ts";
import { denyDesignProofPlugin } from "./build/deny-design-proof.ts";
import { themeInitPlugin } from "./build/theme-init.ts";

/** Injects the derived CSP into the `%VITE_CSP%` placeholder in index.html. */
function cspPlugin(apiBaseUrl: string, livePort: number | null): Plugin {
  return {
    name: "batlehub-csp",
    transformIndexHtml(html) {
      return html.replaceAll("%VITE_CSP%", buildCsp(apiBaseUrl, livePort));
    },
  };
}

export default defineConfig(({ mode }) => {
  // `loadEnv` reads .env files the same way Vite does for `import.meta.env`, so
  // the CSP sees exactly the value the SDK will be built with.
  const env = loadEnv(mode, __dirname, "VITE_");

  return {
    plugins: [
      vue(),
      tailwindcss(),
      // The second argument is the only thing that can widen `script-src`, and
      // `resolveLivePort` returns null for every production build — see
      // `build/csp.ts` and RFC 0003 §7.
      cspPlugin(env.VITE_API_BASE_URL ?? "", resolveLivePort(mode, env)),
      // `ui/design-proof/` sits inside Vite's root without being part of the
      // console, so the dev server published it — see `build/deny-design-proof.ts`.
      denyDesignProofPlugin(),
      // The blocking <head> script that decides the rendition before the first
      // paint. Emitted rather than copied from `public/` so it ships minified
      // — every byte here is a byte before anything is drawn.
      themeInitPlugin(),
    ],
    resolve: {
      alias: {
        "@": path.resolve(__dirname, "./src"),
      },
    },
    server: {
      allowedHosts: [".cde.batleforc.fr", "localhost"],
      host: true,
      // No `fs.deny` here on purpose: a user-declared `deny` *replaces* Vite's
      // defaults rather than extending them, and `ui/.env` came back with a 200
      // the moment one was set. What this server refuses is a middleware
      // instead — `build/deny-design-proof.ts`.
    },
    build: {
      outDir: "dist",
    },
    test: {
      environment: "jsdom",
      setupFiles: ["./src/test/setup.ts"],
      // Mounting a radix Dialog through DialogPortal costs ~600 ms in isolation
      // and was measured past 8 s once the page suites run in parallel — CI
      // parallelises harder than any local run, so the 5 s default turns real
      // passes into flakes that look like product failures.
      testTimeout: 20_000,
      coverage: {
        provider: "v8",
        reporter: ["text", "lcov", "html"],
        // The whole console, not only the files that have a test (RFC 0041 §6.2):
        // a source file nobody tests counts at 0 %, as SonarCloud counts it.
        include: ["src/**/*.{ts,vue}"],
        exclude: [
          "src/client/**",
          "src/**/*.test.ts",
          "src/test/**",
          "src/main.ts",
          "src/components/ui/*/index.ts",
          "src/**/*.d.ts",
        ],
        thresholds: {
          lines: 80,
          branches: 80,
          functions: 80,
          statements: 80,
        },
      },
    },
  };
});
