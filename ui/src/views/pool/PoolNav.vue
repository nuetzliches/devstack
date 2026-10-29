<script setup lang="ts">
// The pool area's top row: this machine (what holds in every pool), then
// each pool joined (ADR-0034), then joining another one.
import { computed } from 'vue';
import { useRoute } from 'vue-router';
import Icon from '@/components/icons/Icon.vue';
import { usePool } from '@/stores/pool';
import { useHub } from '@/stores/hub';

const pool = usePool();
const hub = useHub();
const route = useRoute();
const joined = computed(() => pool.pools.filter((p) => p.joined));
const adding = computed(() => route.query.join !== undefined);
/** A pool's tab keeps the part shown; the machine page goes back to the list in another pool. */
const tabOf = computed(() => (route.name === 'pool-machine' ? 'pool-machines' : String(route.name ?? '').startsWith('pool-') && route.name !== 'pool-here' ? route.name : 'pool-overview'));
</script>

<template>
  <nav v-if="joined.length || hub.guests.length || adding" class="pool-tabs" aria-label="Pools" data-testid="pool-tabs">
    <RouterLink :to="{ name: 'pool-here' }" class="tab" :class="{ on: route.name === 'pool-here' }" :aria-current="route.name === 'pool-here' ? 'page' : undefined" title="What holds for this machine in every pool" data-testid="pool-tab-here">
      <Icon name="monitor" :size="13" /> This machine
    </RouterLink>
    <RouterLink
      v-for="p in joined"
      :key="p.name"
      :to="{ name: tabOf as string, params: { pool: p.name } }"
      class="tab"
      :class="{ on: route.params.pool === p.name && !adding }"
      :aria-current="route.params.pool === p.name && !adding ? 'page' : undefined"
      :title="p.coordinator ?? ''"
      data-testid="pool-tab"
    >
      {{ p.name }}<span v-if="p.state === 'pending'" class="faint"> · waiting</span>
    </RouterLink>
    <RouterLink
      :to="{ name: 'pool-overview', params: { pool: pool.current }, query: { join: '' } }"
      class="tab"
      :class="{ on: adding }"
      title="Join another pool, e.g. a customer's (ADR-0034)"
      data-testid="pool-add"
    >
      <Icon name="plus" :size="13" /> Another pool
    </RouterLink>
  </nav>
</template>

<style scoped>
.pool-tabs {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
}
.tab {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 5px 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface);
  color: var(--text-muted);
  font-size: 13px;
  text-decoration: none;
}
.tab.on {
  border-color: var(--accent);
  color: var(--text);
  background: var(--surface-hover);
}
</style>
