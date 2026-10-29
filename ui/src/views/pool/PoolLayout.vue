<script setup lang="ts">
// The pool area of one pool (ADR-0031, ADR-0032): the pool in the URL is the
// one shown, its parts in tabs. Joining it, or another one, is here too. An
// admin sees what the role allows; the coordinator checks every action.
import { computed, onBeforeUnmount, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import Icon from '@/components/icons/Icon.vue';
import type { IconName } from '@/components/icons/icons';
import { usePool } from '@/stores/pool';
import PoolNav from './PoolNav.vue';
import PoolJoin from './PoolJoin.vue';
import PoolOverview from './PoolOverview.vue';
import './pool.css';

const pool = usePool();
onBeforeUnmount(pool.watch());
const route = useRoute();
const router = useRouter();

watch(
  () => route.params.pool as string | undefined,
  (p) => {
    if (p && p !== pool.current) pool.select(p);
  },
  { immediate: true },
);
// A pool the hub does not know falls back to the default one: the URL follows.
watch(
  () => pool.current,
  (p) => {
    if (route.params.pool && route.params.pool !== p) void router.replace({ name: route.name ?? 'pool-overview', params: { ...route.params, pool: p }, query: route.query });
  },
);

const o = computed(() => pool.overview);
const me = computed(() => o.value?.me);
const adding = computed(() => route.query.join !== undefined);
const nodes = computed(() => o.value?.nodes ?? []);
const tabs = computed(() => {
  const t: { name: string; label: string; icon: IconName; count?: string; due?: number }[] = [
    { name: 'pool-overview', label: 'Overview', icon: 'grid' },
    { name: 'pool-machines', label: 'Machines', icon: 'cpu', count: `${nodes.value.filter((n) => n.online).length}/${pool.machines.length}`, due: pool.due().machines },
    { name: 'pool-lanes', label: 'Lanes', icon: 'gitBranch', count: String((o.value?.leases ?? []).length) },
  ];
  if (pool.isAdmin) t.push({ name: 'pool-admin', label: 'Admin', icon: 'shield', due: pool.due().admin });
  return t;
});
const isActive = (name: string) => route.name === name || (name === 'pool-machines' && route.name === 'pool-machine');
</script>

<template>
  <div class="page pool" data-testid="pool">
    <PoolNav />
    <PoolJoin v-if="adding" another />
    <p v-else-if="pool.error && !o" class="err" data-testid="pool-error"><Icon name="alert" :size="14" /> {{ pool.error }}</p>
    <PoolJoin v-else-if="o && !o.joined" />
    <template v-else-if="me">
      <template v-if="me.state === 'approved'">
        <nav class="subnav" aria-label="Pool views">
          <RouterLink
            v-for="t in tabs"
            :key="t.name"
            :to="{ name: t.name, params: { pool: pool.current } }"
            class="tab"
            :class="{ active: isActive(t.name) }"
            :aria-current="isActive(t.name) ? 'page' : undefined"
            :data-testid="`pool-nav-${t.name.slice(5)}`"
          >
            <Icon :name="t.icon" :size="15" /> {{ t.label }}
            <span v-if="t.count" class="faint tabular">{{ t.count }}</span>
            <span v-if="t.due" class="badge" :title="`${t.due} waiting for an admin here`" :data-testid="`pool-due-${t.name.slice(5)}`">{{ t.due }}</span>
          </RouterLink>
        </nav>
        <RouterView />
      </template>
      <PoolOverview v-else />
    </template>
    <p v-else class="muted">Reading the pool…</p>
  </div>
</template>

<style scoped>
.pool {
  display: flex;
  flex-direction: column;
  gap: 16px;
  max-width: 1800px;
}
.err {
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--danger);
}
.subnav {
  display: flex;
  gap: 2px;
  border-bottom: 1px solid var(--border);
  overflow-x: auto;
  scrollbar-width: none;
}
.tab {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 0 12px;
  height: 40px;
  color: var(--text-muted);
  font-weight: 550;
  font-size: 13px;
  border-bottom: 2px solid transparent;
  white-space: nowrap;
  text-decoration: none;
  margin-bottom: -1px;
}
.tab:hover {
  color: var(--text);
  text-decoration: none;
}
.tab.active {
  color: var(--text);
  border-bottom-color: var(--accent);
}
.tab:focus-visible {
  box-shadow: inset var(--focus);
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
</style>
