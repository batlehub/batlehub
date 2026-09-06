<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import { Terminal, Download, Package, AlertCircle } from "@lucide/vue";
import { useAuth } from "@/composables/useAuth";
import { extractMessage } from "@/composables/useApi";
import { API_BASE_URL } from "@/config";
import { PageHeader } from "@/components/ui/page-header";
import { CopyButton } from "@/components/ui/copy-button";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Alert } from "@/components/ui/alert";

const { t, te } = useI18n();

const { token } = useAuth();

// ── Download URL ──────────────────────────────────────────────────────────────

const downloadUrl = computed(() => `${API_BASE_URL}/api/v1/cli/download`);

// ── Download state ────────────────────────────────────────────────────────────

const downloading = ref(false);
const downloadError = ref<string | null>(null);

async function triggerDownload() {
  downloading.value = true;
  downloadError.value = null;
  try {
    const resp = await fetch(downloadUrl.value, {
      headers: token.value ? { Authorization: `Bearer ${token.value}` } : {},
    });
    if (!resp.ok) {
      const text = await resp.text().catch(() => "");
      const detail = text ? ` — ${text}` : "";
      downloadError.value =
        resp.status === 404
          ? t("cliDownload.binaryNotConfigured")
          : t("cliDownload.downloadFailed", { status: resp.status, detail });
      return;
    }
    const blob = await resp.blob();
    const filename =
      resp.headers.get("Content-Disposition")?.match(/filename="?([^"]+)"?/)?.[1] ?? "batlehub-cli";
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
  } catch (e) {
    downloadError.value = extractMessage(e);
  } finally {
    downloading.value = false;
  }
}

// ── Snippet templates ─────────────────────────────────────────────────────────

const serverUrl = computed(() => globalThis.location.origin);

const RELEASES_URL = "https://github.com/batlehub/batlehub/releases/latest/download";

const installSnippets: Record<string, { label: string; lang: string; code: string }> = {
  mise: {
    label: "mise",
    lang: "bash",
    code: `# Install via mise — manages version automatically (https://mise.jdx.dev)
mise use "github:batleforc/batlehub[asset_pattern=batlehub-cli-*]"`,
  },
  server: {
    label: "cliDownload.fromThisServer",
    lang: "bash",
    get code() {
      return `# Download the binary served by this BatleHub instance
curl -fSL "${serverUrl.value}/api/v1/cli/download" -o batlehub-cli
chmod +x batlehub-cli
./batlehub-cli --version`;
    },
  },
  linux_amd64: {
    label: "Linux x86_64",
    lang: "bash",
    code: `# Linux x86_64 (musl — statically linked, no runtime deps)
curl -fSL "${RELEASES_URL}/batlehub-cli-linux-amd64.tar.gz" | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli`,
  },
  linux_arm64: {
    label: "Linux aarch64",
    lang: "bash",
    code: `# Linux aarch64 (musl — statically linked, no runtime deps)
curl -fSL "${RELEASES_URL}/batlehub-cli-linux-arm64.tar.gz" | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli`,
  },
  macos_arm64: {
    label: "macOS Apple Silicon",
    lang: "bash",
    code: `# macOS aarch64 (Apple Silicon — M1/M2/M3)
curl -fSL "${RELEASES_URL}/batlehub-cli-darwin-arm64.tar.gz" | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli`,
  },
  macos_amd64: {
    label: "macOS Intel",
    lang: "bash",
    code: `# macOS x86_64 (Intel)
curl -fSL "${RELEASES_URL}/batlehub-cli-darwin-amd64.tar.gz" | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli`,
  },
  windows: {
    label: "Windows",
    lang: "powershell",
    code: String.raw`# Windows x86_64 — run in PowerShell
Invoke-WebRequest "${RELEASES_URL}/batlehub-cli-windows-amd64.zip" -OutFile batlehub-cli.zip
Expand-Archive batlehub-cli.zip -DestinationPath .
Move-Item batlehub-cli.exe "$env:LOCALAPPDATA\Microsoft\WindowsApps\batlehub-cli.exe"`,
  },
  cargo: {
    label: "cliDownload.buildFromSource",
    lang: "bash",
    code: `# Requires Rust toolchain (https://rustup.rs)
cargo install --git https://git.batleforc.fr/batleforc/batlehub batlehub-cli

# Or inside the repository:
cargo build -p batlehub-cli --release
# Binary: target/release/batlehub-cli`,
  },
};

const configSnippet = computed(
  () => `[default]
server_url = "${serverUrl.value}"
token      = "your-api-token"`,
);

const usageSnippets = [
  {
    key: "registry",
    label: "cliDownload.listRegistries",
    lang: "bash",
    code: "batlehub-cli registry list",
  },
  {
    key: "whoami",
    label: "cliDownload.checkIdentity",
    lang: "bash",
    code: "batlehub-cli auth whoami",
  },
  {
    key: "list",
    label: "cliDownload.listPackages",
    lang: "bash",
    code: "batlehub-cli package list --registry <name>",
  },
  {
    key: "publish",
    label: "cliDownload.publish",
    lang: "bash",
    code: "batlehub-cli publish MyLib.1.0.0.nupkg --registry <name>",
  },
  {
    key: "yank",
    label: "cliDownload.yankVersion",
    lang: "bash",
    code: "batlehub-cli version yank <registry> <name> <version>",
  },
];
</script>

<template>
  <div class="space-y-6 max-w-3xl">
    <!-- Header -->
    <div class="flex items-center gap-3">
      <Terminal class="h-6 w-6 text-primary shrink-0" />
      <PageHeader title="CLI" class="flex-1">
        <template #description>
          <i18n-t keypath="cliDownload.downloadAndConfigure" tag="span">
            <template #cli><code class="font-mono text-xs">batlehub-cli</code></template>
          </i18n-t>
        </template>
      </PageHeader>
    </div>

    <!-- Download card -->
    <Card>
      <CardHeader>
        <CardTitle class="flex items-center gap-2 text-base">
          <Download class="h-4 w-4" />
          {{ t("common.download") }}
        </CardTitle>
        <CardDescription>{{ t("cliDownload.getThePreBuilt") }}</CardDescription>
      </CardHeader>
      <CardContent class="space-y-4">
        <!-- Error alert -->
        <Alert v-if="downloadError" variant="destructive" class="flex items-start gap-2">
          <AlertCircle class="h-4 w-4 shrink-0 mt-0.5" />
          <p class="text-sm">{{ downloadError }}</p>
        </Alert>

        <!-- Download button -->
        <div class="flex flex-wrap gap-3 items-center">
          <Button class="font-mono gap-2" :disabled="downloading" @click="triggerDownload">
            <Download class="h-4 w-4" />
            {{ downloading ? t("cliDownload.downloading") : t("cliDownload.downloadBatlehubCli") }}
          </Button>
          <span class="text-xs text-muted-foreground font-mono">{{ downloadUrl }}</span>
        </div>

        <!-- Install tabs -->
        <Tabs default-value="download" class="mt-4">
          <TabsList
            class="flex flex-wrap h-auto gap-1 justify-start bg-transparent border-none p-0 mb-2"
          >
            <TabsTrigger
              v-for="(s, key) in installSnippets"
              :key="key"
              :value="key"
              class="font-mono text-xs"
            >
              {{ te(s.label) ? t(s.label) : s.label }}
            </TabsTrigger>
          </TabsList>

          <TabsContent v-for="(s, key) in installSnippets" :key="key" :value="key">
            <div class="relative rounded-sm border border-border bg-muted/40 overflow-hidden">
              <pre
                class="p-4 text-xs font-mono overflow-x-auto text-foreground/90 leading-relaxed"
                >{{ s.code }}</pre>
              <CopyButton
                :text="s.code"
                size="sm"
                variant="ghost"
                class="absolute top-2 right-2 font-mono text-xs h-7 px-2"
              />
            </div>
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>

    <!-- Configuration card -->
    <Card>
      <CardHeader>
        <CardTitle class="flex items-center gap-2 text-base">
          <Package class="h-4 w-4" />
          {{ t("common.configuration") }}
        </CardTitle>
        <CardDescription>
          <i18n-t keypath="cliDownload.createConfigOrRun" tag="span">
            <template #path
              ><code class="font-mono text-xs">~/.config/batlehub/config.toml</code></template
            >
            <template #command
              ><code class="font-mono text-xs">batlehub-cli config init</code></template
            >
          </i18n-t>
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-4">
        <div class="relative rounded-sm border border-border bg-muted/40 overflow-hidden">
          <pre class="p-4 text-xs font-mono overflow-x-auto text-foreground/90 leading-relaxed">{{
            configSnippet
          }}</pre>
          <CopyButton
            :text="configSnippet"
            size="sm"
            variant="ghost"
            class="absolute top-2 right-2 font-mono text-xs h-7 px-2"
          />
        </div>

        <p class="text-xs text-muted-foreground">
          {{ t("cliDownload.overrideWithEnv") }}
          <code class="font-mono">BATLEHUB_SERVER</code>,
          <code class="font-mono">BATLEHUB_TOKEN</code>,
          <code class="font-mono">BATLEHUB_REGISTRY</code>.
        </p>
      </CardContent>
    </Card>

    <!-- Quick reference card -->
    <Card>
      <CardHeader>
        <CardTitle class="flex items-center gap-2 text-base">
          <Terminal class="h-4 w-4" />
          {{ t("cliDownload.quickReference") }}
        </CardTitle>
        <CardDescription>{{ t("cliDownload.commonCommandsToGet") }}</CardDescription>
      </CardHeader>
      <CardContent>
        <div class="space-y-3">
          <div
            v-for="s in usageSnippets"
            :key="s.key"
            class="relative rounded-sm border border-border bg-muted/40 overflow-hidden"
          >
            <div class="px-3 pt-2 pb-0.5 text-xs text-muted-foreground font-mono">
              {{ te(s.label) ? t(s.label) : s.label }}
            </div>
            <pre class="px-3 pb-3 pt-1 text-xs font-mono text-foreground/90 leading-relaxed">{{
              s.code
            }}</pre>
            <CopyButton
              :text="s.code"
              size="sm"
              variant="ghost"
              class="absolute top-1 right-2 font-mono text-xs h-7 px-2"
            />
          </div>
        </div>
      </CardContent>
    </Card>
  </div>
</template>
