<script setup lang="ts">
// `/`: go to the last selected stack, else the first running one.
import { computed, watchEffect } from 'vue';
import { useRouter } from 'vue-router';
import { persist } from '@/lib/persist';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';

const hub = useHub();
const router = useRouter();

const target = computed(() => {
  const last = persist.lastStack();
  if (last && (hub.entry(last) || persist.stack(last))) return last;
  return hub.stacks.find((s) => s.running)?.id ?? hub.stacks[0]?.id ?? null;
});

watchEffect(() => {
  if (target.value) void router.replace({ name: 'overview', params: { stack: target.value } });
});
</script>

<template>
  <div class="page">
    <div v-if="!target" class="card">
      <div class="empty">
        <Icon name="layers" :size="36" />
        <h1>No stacks yet</h1>
        <p>A stack appears here when its daemon starts. Run <code>devstack up</code> in a project with a <code>devstack.toml</code>.</p>
      </div>
    </div>
  </div>
</template>
