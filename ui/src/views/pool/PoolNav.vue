<script setup lang="ts">
// The pool area's top row: one switch between this machine (what holds in
// every pool) and each pool joined (ADR-0034), then, apart from it, the
// action of joining another one.
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
const here = computed(() => route.name === 'pool-here');
const isPool = (name: string) => route.params.pool === name && !adding.value;
</script>

<template>
  <nav v-if="joined.length || hub.guests.length || adding" class="pool-nav" aria-label="Pools" data-testid="pool-tabs">
    <div class="switch">
      <RouterLink
        :to="{ name: 'pool-here' }"
        class="seg"
        :class="{ on: here }"
        :aria-current="here ? 'page' : undefined"
        title="What holds for this machine in every pool"
        data-testid="pool-tab-here"
      >
        <Icon name="monitor" :size="14" /> This machine
      </RouterLink>
      <RouterLink
        v-for="p in joined"
        :key="p.name"
        :to="{ name: tabOf as string, params: { pool: p.name } }"
        class="seg"
        :class="{ on: isPool(p.name) }"
        :aria-current="isPool(p.name) ? 'page' : undefined"
        :title="p.coordinator ?? ''"
        data-testid="pool-tab"
      >
        <Icon name="server" :size="14" /> {{ p.name }}<span v-if="p.state === 'pending'" class="faint"> · waiting</span>
        <span v-if="pool.due(p.name).all" class="badge" :title="`${pool.due(p.name).all} waiting for an admin in ${p.name}`" data-testid="pool-tab-due">{{ pool.due(p.name).all }}</span>
      </RouterLink>
    </div>
    <RouterLink
      :to="{ name: 'pool-overview', params: { pool: pool.current }, query: { join: '' } }"
      class="join"
      :class="{ on: adding }"
      :aria-current="adding ? 'page' : undefined"
      title="Join another pool, e.g. a customer's (ADR-0034)"
      data-testid="pool-add"
    >
      <Icon name="plus" :size="14" /> Join another pool
    </RouterLink>
  </nav>
</template>

<style scoped>
.pool-nav {
  display: flex;
  flex-wrap: wrap;
  align-items: stretch;
  gap: 12px;
}
/* The action beside the switch: as tall as it, framed the same, so it
   reads as one row; its border and plus say it does something. */
.join {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 0 14px;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface);
  color: var(--text);
  font-size: 13px;
  font-weight: 550;
  text-decoration: none;
}
.join:hover {
  border-color: var(--accent);
  background: var(--surface-hover);
  text-decoration: none;
}
.join.on {
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 18%, var(--surface));
}
.join:focus-visible {
  box-shadow: var(--focus);
}
/* One control, segments side by side: picking one shows it. */
.switch {
  display: inline-flex;
  flex-wrap: wrap;
  padding: 3px;
  gap: 2px;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface);
}
.seg {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 5px 12px;
  border-radius: 6px;
  color: var(--text-muted);
  font-size: 13px;
  font-weight: 550;
  text-decoration: none;
}
.seg:hover {
  color: var(--text);
  background: var(--surface-hover);
  text-decoration: none;
}
.seg.on {
  color: var(--text);
  background: color-mix(in srgb, var(--accent) 18%, var(--surface));
  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 55%, transparent);
}
.badge {
  min-width: 18px;
  height: 18px;
  padding: 0 5px;
  border-radius: 999px;
  background: var(--warn);
  color: #1a1204;
  font-size: 11px;
  font-weight: 700;
  display: inline-grid;
  place-items: center;
}
.seg:focus-visible {
  box-shadow: var(--focus);
}
</style>
