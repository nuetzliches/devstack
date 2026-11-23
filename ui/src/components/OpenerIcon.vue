<script setup lang="ts">
// A program's own icon, as this machine shows it (`GET /hub/openers/{id}/icon`,
// ADR-0046 addendum 2026-11-23): the hub reads it from the installed program,
// so no vendor's logo ships with the dashboard. Where the hub has none (macOS
// and Linux yet, the mock hub, a program without one), the kind's icon.
import { ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { Opener } from '@/api/types';
import Icon from './icons/Icon.vue';

const props = withDefaults(defineProps<{ opener: Opener; size?: number }>(), { size: 14 });

const own = ref(hasOwn(props.opener.id));
watch(
  () => props.opener.id,
  (id) => (own.value = hasOwn(id)),
);

function hasOwn(id: string): boolean {
  return api().kind === 'real' && !missing.has(id);
}
function failed() {
  missing.add(props.opener.id);
  own.value = false;
}
</script>

<script lang="ts">
/** The icon of a program by its kind, where it has no own. */
export const kindIcon = {
  files: 'folder',
  terminal: 'terminal',
  editor: 'file',
  git: 'gitBranch',
  agent: 'zap',
} as const;

/** Programs whose icon the hub did not have: asked once per page. */
const missing = new Set<string>();
</script>

<template>
  <img
    v-if="own"
    :src="paths.openerIcon(opener.id)"
    :width="size"
    :height="size"
    alt=""
    aria-hidden="true"
    class="opener-icon"
    data-testid="opener-icon"
    @error="failed"
  />
  <Icon v-else :name="kindIcon[opener.kind] ?? 'external'" :size="size" />
</template>

<style scoped>
.opener-icon {
  flex: none;
  object-fit: contain;
}
</style>
