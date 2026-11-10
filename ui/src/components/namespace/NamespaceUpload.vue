<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import type { RegistryInfo } from "@/client/types.gen";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { extractMessage } from "@/composables/useApi";
import { API_BASE_URL } from "@/config";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { CodeBlock } from "@/components/ui/code-block";

const { t } = useI18n();

const props = defineProps<{ registries: RegistryInfo[] }>();

const { authFetch } = useAuthFetch();

const uploadableRegistries = computed(() =>
  props.registries.filter((r) => r.mode === "local" || r.mode === "hybrid"),
);

const selectedRegistry = ref("");
const registryOptions = computed(() =>
  uploadableRegistries.value.map((r) => ({ value: r.name, label: r.name })),
);
const registryType = computed(
  () => props.registries.find((r) => r.name === selectedRegistry.value)?.type ?? "",
);

/**
 * Base URL for `name`: the registry's own hostname-rooted URL when the server
 * advertises one (`public_url`, set by host-based routing), otherwise the
 * `/proxy/{name}` subpath. Callers append paths and never write `/proxy/`.
 */
function urlFor(name: string, origin: string = API_BASE_URL): string {
  return props.registries.find((r) => r.name === name)?.public_url ?? `${origin}/proxy/${name}`;
}

const BINARY_TYPES = new Set([
  "rubygems",
  "composer",
  "openvsx",
  "vscode-marketplace",
  "goproxy",
  "deb",
  "rpm",
]);
const isBinaryUpload = computed(() => BINARY_TYPES.has(registryType.value));

const uploadFile = ref<File | null>(null);
const uploadExtId = ref("");
const uploadVersion = ref("");
const uploadModule = ref("");
const uploadDistribution = ref("");
const uploadComponent = ref("main");
const loading = ref(false);
const error = ref<string | null>(null);
const success = ref(false);

function onFileChange(e: Event) {
  uploadFile.value = (e.target as HTMLInputElement).files?.[0] ?? null;
}

function acceptFor(t: string) {
  if (t === "rubygems") return ".gem";
  if (t === "composer") return ".zip";
  if (t === "openvsx" || t === "vscode-marketplace") return ".vsix";
  if (t === "goproxy") return ".zip";
  if (t === "deb") return ".deb";
  if (t === "rpm") return ".rpm";
  return "*";
}

/** Build the upload URL + HTTP method for the selected registry type.
 *  Throws when type-specific required fields are missing. */
function buildUploadTarget(type: string, reg: string): { url: string; method: string } {
  const base = urlFor(reg);
  switch (type) {
    case "rubygems":
      return { url: `${base}/api/v1/gems`, method: "POST" };
    case "composer":
      return { url: `${base}/api/upload`, method: "POST" };
    case "openvsx":
    case "vscode-marketplace": {
      const id = uploadExtId.value.trim();
      const version = uploadVersion.value.trim();
      if (!id || !version) throw new Error(t("namespaceUpload.extensionIdAndVersionRequired"));
      return {
        url: `${base}/${encodeURIComponent(id)}/${encodeURIComponent(version)}/vsix`,
        method: "PUT",
      };
    }
    case "goproxy": {
      const mod = uploadModule.value.trim();
      const version = uploadVersion.value.trim();
      if (!mod || !version) throw new Error(t("namespaceUpload.modulePathAndVersionRequired"));
      return {
        url: `${base}/${encodeURIComponent(mod)}/@v/${encodeURIComponent(version)}.zip`,
        method: "PUT",
      };
    }
    case "deb": {
      const dist = uploadDistribution.value.trim();
      const component = uploadComponent.value.trim();
      if (!dist || !component)
        throw new Error(t("namespaceUpload.distributionAndComponentRequired"));
      return {
        url: `${base}/deb/pool/${encodeURIComponent(dist)}/${encodeURIComponent(component)}/upload`,
        method: "PUT",
      };
    }
    case "rpm":
      return { url: `${base}/rpm/upload`, method: "PUT" };
    default:
      return { url: "", method: "POST" };
  }
}

async function doUpload() {
  if (!uploadFile.value || !selectedRegistry.value) return;
  loading.value = true;
  error.value = null;
  success.value = false;
  try {
    const reg = encodeURIComponent(selectedRegistry.value);
    const { url, method } = buildUploadTarget(registryType.value, reg);
    const body = await uploadFile.value.arrayBuffer();
    const r = await authFetch(url, {
      method,
      headers: { "Content-Type": "application/octet-stream" },
      body,
    });
    if (!r.ok) throw new Error(await r.text());
    success.value = true;
    uploadFile.value = null;
    uploadExtId.value = "";
    uploadVersion.value = "";
    uploadModule.value = "";
    uploadDistribution.value = "";
    setTimeout(() => {
      success.value = false;
    }, 4000);
  } catch (e_) {
    error.value = extractMessage(e_);
  } finally {
    loading.value = false;
  }
}

const cliRegistryName = computed(() => selectedRegistry.value || "<registry>");
/** The selected registry's client-facing base URL, on this page's origin. */
const cliRegistryUrl = computed(() => urlFor(cliRegistryName.value, globalThis.location.origin));
/**
 * Reactive: both the registry name and its base URL change with the selector,
 * so this cannot be a plain object evaluated once during setup.
 */
const cliSnippets = computed<Record<string, string>>(() => {
  const reg = cliRegistryName.value;
  const url = cliRegistryUrl.value;
  return {
    npm: `npm set registry ${url}\nnpm publish`,
    cargo: `# .cargo/config.toml:\n[registries.${reg}]\nindex = "sparse+${url}/registry/"\n\ncargo publish --registry ${reg}`,
    maven: `<!-- settings.xml -->\n<server>\n  <id>${reg}</id>\n  <username>your-user</username>\n  <password>your-token</password>\n</server>\n\n<!-- pom.xml -->\n<distributionManagement>\n  <repository>\n    <id>${reg}</id>\n    <url>${url}/maven2</url>\n  </repository>\n</distributionManagement>\n\nmvn deploy`,
    terraform: `terraform {\n  required_providers {\n    <provider> = {\n      source = "${terraformSource(url, reg)}/<namespace>/<provider>"\n    }\n  }\n}`,
    deb: `# Publish a .deb to pool/{distribution}/{component}\ncurl -X PUT \\\n  -H "Authorization: Bearer <your-token>" \\\n  --data-binary @hello_1.0_amd64.deb \\\n  ${url}/deb/pool/stable/main/upload`,
    rpm: `# Publish an .rpm\ncurl -X PUT \\\n  -H "Authorization: Bearer <your-token>" \\\n  --data-binary @hello-1.0-1.x86_64.rpm \\\n  ${url}/rpm/upload`,
  };
});

/**
 * Terraform provider sources are keyed by host, not URL. `host` rather than
 * `hostname`, so a non-default port survives — a source of `localhost/ns/name`
 * sends `terraform init` to port 443 on a dev server listening on 8080.
 */
function terraformHost(url: string): string {
  try {
    return new URL(url, globalThis.location.origin).host;
  } catch {
    return globalThis.location.host;
  }
}

/**
 * `//a/b//` → `a/b`.
 *
 * Walked rather than `replace(/^\/+|\/+$/g, "")`: an anchored `\/+$` re-scans a
 * run of slashes from every position inside it, which is quadratic on a path
 * that is mostly slashes.
 */
function trimSlashes(value: string): string {
  let start = 0;
  let end = value.length;
  while (start < end && value[start] === "/") start++;
  while (end > start && value[end - 1] === "/") end--;
  return value.slice(start, end);
}

/**
 * The `host[/registry]` prefix of a provider source. A host-routed registry
 * serves `/v1/providers/…` at the root of its own hostname, so repeating the
 * registry name there points `terraform init` at a path that does not exist.
 */
function terraformSource(url: string, reg: string): string {
  let path: string;
  try {
    path = trimSlashes(new URL(url, globalThis.location.origin).pathname);
  } catch {
    path = "";
  }
  return path ? `${terraformHost(url)}/${reg}` : terraformHost(url);
}

const currentSnippet = computed(
  () => cliSnippets.value[registryType.value] ?? t("namespaceUpload.noCliInstructions"),
);
</script>

<template>
  <div v-if="!uploadableRegistries.length" class="text-sm text-muted-foreground">
    {{ t("namespaceUpload.noRegistriesInLocal") }}
  </div>
  <template v-else>
    <div class="space-y-2 w-60">
      <Label for="upload-registry">{{ t("common.registry") }}</Label>
      <Select
        id="upload-registry"
        v-model="selectedRegistry"
        :options="registryOptions"
        :placeholder="t('namespaceUpload.selectRegistry')"
      />
    </div>

    <Tabs :default-value="isBinaryUpload ? 'upload' : 'cli'" class="w-full">
      <TabsList>
        <TabsTrigger v-if="isBinaryUpload" value="upload">{{
          t("namespaceUpload.fileUpload")
        }}</TabsTrigger>
        <TabsTrigger value="cli">{{ t("namespaceUpload.cliInstructions") }}</TabsTrigger>
      </TabsList>

      <TabsContent v-if="isBinaryUpload" value="upload" class="space-y-4 pt-4">
        <template v-if="registryType === 'openvsx' || registryType === 'vscode-marketplace'">
          <div class="grid grid-cols-2 gap-3">
            <div class="space-y-2">
              <Label for="upload-ext-id"
                >{{ t("namespaceUpload.extensionId") }}
                <span class="text-muted-foreground text-xs">{{
                  t("namespaceUpload.publisherName")
                }}</span></Label
              >
              <Input
                id="upload-ext-id"
                v-model="uploadExtId"
                placeholder="ms-python.python"
                class="font-mono text-sm"
              />
            </div>
            <div class="space-y-2">
              <Label for="upload-version-ext">{{ t("common.version") }}</Label>
              <Input
                id="upload-version-ext"
                v-model="uploadVersion"
                placeholder="1.0.0"
                class="font-mono text-sm"
              />
            </div>
          </div>
        </template>
        <template v-else-if="registryType === 'goproxy'">
          <div class="grid grid-cols-2 gap-3">
            <div class="space-y-2">
              <Label for="upload-module">{{ t("namespaceUpload.modulePath") }}</Label>
              <Input
                id="upload-module"
                v-model="uploadModule"
                placeholder="github.com/org/repo"
                class="font-mono text-sm"
              />
            </div>
            <div class="space-y-2">
              <Label for="upload-version-module">{{ t("common.version") }}</Label>
              <Input
                id="upload-version-module"
                v-model="uploadVersion"
                placeholder="v1.0.0"
                class="font-mono text-sm"
              />
            </div>
          </div>
        </template>
        <template v-else-if="registryType === 'deb'">
          <div class="grid grid-cols-2 gap-3">
            <div class="space-y-2">
              <Label for="upload-distribution"
                >{{ t("common.distribution") }}
                <span class="text-muted-foreground text-xs">{{
                  t("namespaceUpload.suite")
                }}</span></Label
              >
              <Input
                id="upload-distribution"
                v-model="uploadDistribution"
                placeholder="stable"
                class="font-mono text-sm"
              />
            </div>
            <div class="space-y-2">
              <Label for="upload-component">{{ t("common.component") }}</Label>
              <Input
                id="upload-component"
                v-model="uploadComponent"
                placeholder="main"
                class="font-mono text-sm"
              />
            </div>
          </div>
        </template>

        <div class="space-y-2">
          <Label for="upload-file"
            >{{ t("common.file") }}
            <span class="text-muted-foreground text-xs ml-1">{{
              acceptFor(registryType)
            }}</span></Label
          >
          <input
            id="upload-file"
            type="file"
            :accept="acceptFor(registryType)"
            class="block text-sm text-muted-foreground file:mr-3 file:py-2 file:px-3 file:rounded-sm file:border-0 file:text-xs file:font-medium file:bg-ground-raised file:text-foreground cursor-pointer"
            @change="onFileChange"
          />
        </div>

        <div class="flex items-center gap-3">
          <Button :disabled="!uploadFile || loading" @click="doUpload">
            {{ loading ? t("namespaceUpload.uploading") : t("namespaceUpload.upload") }}
          </Button>
          <!-- Full ink, not `text-primary`: that resolves to `--accent`, the same
               crimson as the failure line below it, so a publish that worked and
               a publish that did not rendered identically. -->
          <span v-if="success" class="text-sm text-foreground">{{
            t("namespaceUpload.publishedSuccessfully")
          }}</span>
          <span v-if="error" class="text-sm text-destructive">{{ error }}</span>
        </div>
      </TabsContent>

      <TabsContent value="cli" class="pt-4">
        <CodeBlock :code="currentSnippet" lang="bash" />
      </TabsContent>
    </Tabs>
  </template>
</template>
