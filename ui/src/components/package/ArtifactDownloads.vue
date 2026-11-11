<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { Download, FileCode, FileJson } from "@lucide/vue";
import type { ExploreVersionDto } from "@/client/types.gen";
import { useAuth } from "@/composables/useAuth";
import { API_BASE_URL } from "@/config";

/** The selected version's artifact download and SBOM buttons. The SBOM state is
    the page's, so a 404 remembered there outlives this component remounting. */
const props = defineProps<{
  registry: string;
  name: string;
  registryType: string | null;
  selectedRow: ExploreVersionDto;
  sbomFormats: string[];
  sbomState: "available" | "none" | "unknown";
  sbomLoading: string | null;
  sbomError?: string;
}>();

const emit = defineEmits<{ "download-sbom": [version: string, fmt: "spdx" | "cyclonedx"] }>();

const { t } = useI18n();
const { token } = useAuth();

// ── Download URL construction ──────────────────────────────────────────────────

/**
 * Build the proxy download URL for a given version based on the registry type.
 * Returns null for registries whose URL can't be derived purely from name/version
 * (Maven, PyPI simple page, etc.).
 */
function downloadUrl(version: string): string | null {
  const n = props.name;
  const r = props.registry;
  const base = `${API_BASE_URL}/proxy/${encodeURIComponent(r)}`;
  switch (props.registryType) {
    case "cargo":
      return `${base}/${encodeURIComponent(n)}/${encodeURIComponent(version)}/download`;
    case "npm":
      // encodeURIComponent handles scoped packages: @scope/pkg → %40scope%2Fpkg (one path segment)
      return `${base}/${encodeURIComponent(n)}/${encodeURIComponent(version)}/tarball`;
    case "nuget":
      return `${base}/nuget/v3/flat/${encodeURIComponent(n.toLowerCase())}/${encodeURIComponent(version.toLowerCase())}/${encodeURIComponent(n.toLowerCase())}.${encodeURIComponent(version.toLowerCase())}.nupkg`;
    case "rubygems":
      return `${base}/gems/${encodeURIComponent(n)}-${encodeURIComponent(version)}.gem`;
    case "pypi":
      // PyPI has hashed filenames — link to the simple page instead
      return `${base}/simple/${encodeURIComponent(n)}/`;
    case "conda":
      return `${base}/noarch/${encodeURIComponent(n)}-${encodeURIComponent(version)}-py_0.conda`;
    case "vsix":
    case "openvsx": {
      const parts = n.split(".");
      if (parts.length === 2) {
        return `${base}/${encodeURIComponent(parts[0])}.${encodeURIComponent(parts[1])}/${encodeURIComponent(version)}/vsix`;
      }
      return null;
    }
    default:
      return null;
  }
}
</script>

<template>
  <!-- The artifact itself and its bill of materials, for the version in
       hand. These were three chips repeated on every row of a nine-column
       table — where the Download one rendered a bare em dash for thirteen
       of the twenty-one registry types, under a column headed "Download",
       stating nothing about why. Here they are absent when there is
       nothing to offer, which is the same answer without the furniture. -->
  <div class="mt-4 flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
    <a
      v-if="downloadUrl(selectedRow.version)"
      :href="downloadUrl(selectedRow.version)!"
      target="_blank"
      rel="noopener noreferrer"
      class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px]"
    >
      <Download class="h-4 w-4" aria-hidden="true" />
      {{ t("packageDetailPage.downloadViaProxy", { version: selectedRow.version }) }}
    </a>
    <template v-if="token && sbomState === 'available'">
      <button
        v-if="sbomFormats.includes('spdx')"
        type="button"
        class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px] disabled:no-underline disabled:opacity-50"
        :disabled="sbomLoading === `${registry}/${name}/${selectedRow.version}:spdx`"
        @click="emit('download-sbom', selectedRow!.version, 'spdx')"
      >
        <FileJson class="h-4 w-4" aria-hidden="true" />
        {{ t("packageDetailPage.downloadSpdx23") }}
      </button>
      <button
        v-if="sbomFormats.includes('cyclonedx')"
        type="button"
        class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px] disabled:no-underline disabled:opacity-50"
        :disabled="sbomLoading === `${registry}/${name}/${selectedRow.version}:cyclonedx`"
        @click="emit('download-sbom', selectedRow!.version, 'cyclonedx')"
      >
        <FileCode class="h-4 w-4" aria-hidden="true" />
        {{ t("packageDetailPage.downloadCyclonedx14") }}
      </button>
    </template>
    <span v-else-if="token && sbomState === 'none'" class="text-muted-foreground">{{
      t("packageDetailPage.noSbom")
    }}</span>
    <span v-else-if="token" class="text-muted-foreground">{{
      t("packageDetailPage.sbomPending")
    }}</span>
  </div>
  <output v-if="sbomError" class="mt-2 block text-sm text-destructive">
    {{
      t("packageDetailPage.sbomFailed", {
        reason: sbomError,
      })
    }}
  </output>
</template>
