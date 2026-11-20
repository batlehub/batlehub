<script setup lang="ts">
import { ShieldCheck } from "@lucide/vue";
import ConsoleLayout from "./ConsoleLayout.vue";
import { ADMIN_SECTIONS } from "@/config/adminSections";
</script>

<template>
  <ConsoleLayout
    :sections="ADMIN_SECTIONS"
    title="common.admin"
    :icon="ShieldCheck"
    nav-key="admin"
  />
</template>
