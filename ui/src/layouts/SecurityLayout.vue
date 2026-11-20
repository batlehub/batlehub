<script setup lang="ts">
import { Shield } from "@lucide/vue";
import ConsoleLayout from "./ConsoleLayout.vue";
import { SECURITY_SECTIONS } from "@/config/securitySections";
</script>

<template>
  <ConsoleLayout
    :sections="SECURITY_SECTIONS"
    title="nav.security"
    :icon="Shield"
    nav-key="security"
  />
</template>
