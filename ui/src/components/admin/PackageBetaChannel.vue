<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref } from "vue";
import { listBetaMembers } from "@/client/sdk.gen";
import type { BetaChannelMemberDto } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardHeader, CardContent } from "@/components/ui/card";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

const props = defineProps<{ registry: string }>();

const { token } = useAuth();
const expanded = ref(false);

const {
  data: members,
  loading,
  reload,
} = useApi<BetaChannelMemberDto[]>(() => {
  if (!props.registry) return Promise.resolve({ data: [] });
  return listBetaMembers({ path: { registry: props.registry } }) as Promise<{
    data?: unknown;
    error?: unknown;
  }>;
}, [token]);
</script>

<template>
  <Card>
    <CardHeader>
      <div class="flex items-center justify-between">
        <button
          type="button"
          class="flex items-center gap-2 text-base font-semibold hover:text-primary transition-colors"
          :aria-expanded="expanded"
          :aria-controls="expanded ? 'beta-channel-members' : undefined"
          @click="expanded = !expanded"
        >
          {{ t("packageBetaChannel.betaChannelAccess") }}
          <span class="text-muted-foreground text-xs font-normal">{{
            expanded ? t("packageBetaChannel.hide") : t("packageBetaChannel.show")
          }}</span>
          <Badge v-if="members && members.length > 0" variant="secondary" class="text-xs ml-1">
            {{ t("packageBetaChannel.memberCount", members.length) }}
          </Badge>
        </button>
        <Button v-if="expanded" variant="outline" size="sm" :disabled="loading" @click="reload">
          {{ loading ? t("common.loading") : t("common.refresh") }}
        </Button>
      </div>
    </CardHeader>
    <CardContent v-if="expanded" id="beta-channel-members" class="p-0">
      <p class="px-6 py-2 text-xs text-muted-foreground border-b">
        {{ t("packageBetaChannel.preReleaseVersionsAre") }}
      </p>
      <Table :label="t('a11y.betaMembersTableLabel')">
        <TableHeader>
          <TableRow>
            <TableHead>{{ t("common.type") }}</TableHead>
            <TableHead>{{ t("packageBetaChannel.principalId") }}</TableHead>
            <TableHead>{{ t("packageBetaChannel.grantedBy") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="m in members" :key="m.principal_type + ':' + m.principal_id">
            <TableCell>
              <Badge
                :variant="m.principal_type === 'user' ? 'known' : 'secondary'"
                class="text-xs capitalize"
              >
                {{ m.principal_type }}
              </Badge>
            </TableCell>
            <TableCell class="font-mono text-sm">{{ m.principal_id }}</TableCell>
            <TableCell class="text-sm text-muted-foreground">{{ m.granted_by ?? "—" }}</TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <p
        v-if="!members || members.length === 0"
        class="p-6 text-sm text-muted-foreground text-center"
      >
        {{ t("packageBetaChannel.noBetaChannelMembers") }}
      </p>
    </CardContent>
  </Card>
</template>
