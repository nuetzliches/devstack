<script setup lang="ts">
// `/`: every stack as a tile, in the sidebar's order (favourites first,
// hidden stacks left out); with no stack yet, the way to find them.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { storeToRefs } from 'pinia';
import type { StackEntry } from '@/api/types';
import { useHub } from '@/stores/hub';
import { useUi, type HomeState } from '@/stores/ui';
import Icon from '@/components/icons/Icon.vue';
import ScanStacks from '@/components/ScanStacks.vue';
import AddStackDialog from '@/components/AddStackDialog.vue';
import OpenersDialog from '@/components/OpenersDialog.vue';
import StackTile from '@/components/StackTile.vue';
import { matchesQuery, openedTimes, orderStacks, savedOrder, useFavourites, useHidden } from '@/lib/stackOrder';

const hub = useHub();
const addOne = ref(false);
const managing = ref(false);
const favourites = useFavourites();
const hidden = useHidden();

// The order the sidebar has, read when the page opens; last opened and last
// edited are snapshots there too, so nothing moves under the pointer. A
// stack whose commit time is first known later joins with it.
const order = savedOrder();
const opened = openedTimes();
const edited = ref<Record<string, number>>({});
watch(
  () => hub.stacks.map((s) => `${s.id}:${s.last_commit ?? ''}`).join(','),
  () => {
    const next = { ...edited.value };
    for (const s of hub.stacks) if (s.last_commit != null && !(s.id in next)) next[s.id] = s.last_commit;
    edited.value = next;
  },
  { immediate: true },
);

// A stack that runs is never hidden: it would be missed (as in the sidebar).
const isHidden = (id: string, running: boolean) => hidden.has(id) && !running;
const listed = computed(() => hub.stacks.filter((s) => !isHidden(s.id, s.running)));
const attentionIds = computed(() => new Set(hub.attention.map((a) => a.stack)));
const running = computed(() => hub.stacks.filter((s) => s.running).length);
const needing = computed(() => attentionIds.value.size);

// The search and the state filter (kept in the ui store, so the way to a
// stack and back through the mark finds them as they were left). The chips'
// figures count the listed stacks whatever is typed.
const ui = useUi();
const { homeQuery: query, homeState: state } = storeToRefs(ui);
const inState = (s: StackEntry, which: HomeState) => which === 'all' || (which === 'running' ? s.running : attentionIds.value.has(s.id));
const chips = computed(() => [
  { key: 'all' as const, label: 'All', count: listed.value.length },
  { key: 'running' as const, label: 'Running', count: listed.value.filter((s) => s.running).length },
  { key: 'attention' as const, label: 'Needs attention', count: listed.value.filter((s) => attentionIds.value.has(s.id)).length },
]);
const filtering = computed(() => query.value.trim() !== '' || state.value !== 'all');
const shown = computed(() =>
  orderStacks(
    listed.value.filter((s) => inState(s, state.value) && matchesQuery(s, query.value)),
    order,
    opened,
    favourites.list(),
    edited.value,
  ),
);
// Hidden stacks stay out; while something is typed, the line says how many
// of them it would have found, so a search never denies a stack that exists.
const hiddenAll = computed(() => hub.stacks.filter((s) => isHidden(s.id, s.running)));
const hiddenMatching = computed(() => hiddenAll.value.filter((s) => matchesQuery(s, query.value)).length);
const hiddenCount = computed(() => hiddenAll.value.length);

function clearFilter() {
  query.value = '';
  state.value = 'all';
}

const searchEl = ref<HTMLInputElement | null>(null);
// `/` jumps to the search unless a field has the keys already.
function onKey(e: KeyboardEvent) {
  if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey) return;
  if (e.target instanceof Element && e.target.closest('input, textarea, select, [contenteditable]')) return;
  e.preventDefault();
  searchEl.value?.focus();
}
function onSearchEsc() {
  if (query.value) query.value = '';
  else searchEl.value?.blur();
}
onMounted(() => window.addEventListener('keydown', onKey));
onBeforeUnmount(() => window.removeEventListener('keydown', onKey));

function manage() {
  void hub.loadOpeners();
  managing.value = true;
}
</script>

<template>
  <div class="page">
    <section v-if="!hub.stacks.length" class="card welcome" data-testid="welcome">
      <header class="card-header">
        <Icon name="layers" :size="18" />
        <h1>Find your stacks</h1>
      </header>
      <div class="card-body">
        <p>
          Choose the folder your repositories live in (for example <code>D:\Sources</code>): devstack lists every repository with a
          <code>devstack.toml</code> and adds the ones you want. Starting them is one click afterwards.
        </p>
        <ScanStacks />
        <p class="muted alt">
          A single repository, or one without a <code>devstack.toml</code> yet (devstack can write a first draft):
          <button type="button" class="btn btn-sm" data-testid="welcome-add-one" @click="addOne = true"><Icon name="plus" :size="14" /> Add one repository</button>
          · or run <code>devstack up</code> in it.
        </p>
      </div>
    </section>

    <template v-else>
      <header class="overview-head">
        <div class="title">
          <h1>Stacks</h1>
          <span class="muted counts" data-testid="overview-counts">
            {{ hub.stacks.length }} {{ hub.stacks.length === 1 ? 'stack' : 'stacks' }} · {{ running }} running<template v-if="needing">
              · <span class="attn">{{ needing }} {{ needing === 1 ? 'needs' : 'need' }} attention</span></template
            >
          </span>
        </div>
        <div class="actions">
          <button type="button" class="btn btn-sm" data-testid="overview-programs" @click="manage"><Icon name="external" :size="14" /> Manage programs…</button>
          <button type="button" class="btn btn-sm" data-testid="overview-add" @click="addOne = true"><Icon name="plus" :size="14" /> Add a stack</button>
        </div>
      </header>

      <div class="filters" data-testid="overview-filters">
        <span class="search">
          <Icon name="search" :size="14" class="muted" />
          <input
            ref="searchEl"
            v-model="query"
            class="input"
            type="search"
            placeholder="Search stacks by name or folder"
            aria-label="Search stacks"
            title="Press / to search"
            spellcheck="false"
            data-testid="overview-search"
            @keydown.esc.prevent="onSearchEsc"
          />
        </span>
        <div class="chips" role="group" aria-label="Filter by state">
          <button
            v-for="c in chips"
            :key="c.key"
            type="button"
            class="chip-toggle"
            :aria-pressed="state === c.key"
            :data-testid="`overview-chip-${c.key}`"
            @click="state = c.key"
          >
            {{ c.label }} <span class="faint tabular">{{ c.count }}</span>
          </button>
        </div>
      </div>

      <div v-if="shown.length" class="tiles" data-testid="overview-tiles">
        <StackTile v-for="s in shown" :key="s.id" :stack="s" :favourite="favourites.has(s.id)" @toggle-favourite="favourites.toggle(s.id)" />
      </div>
      <p v-else-if="filtering" class="muted empty" data-testid="overview-empty">
        No stack matches.
        <button type="button" class="link" data-testid="overview-clear" @click="clearFilter">Clear the filter</button>
      </p>
      <p v-if="query.trim() && hiddenMatching > 0" class="faint small hidden-note" data-testid="overview-hidden">
        {{ hiddenMatching }} hidden {{ hiddenMatching === 1 ? 'stack matches' : 'stacks match' }}; the sidebar lists {{ hiddenMatching === 1 ? 'it' : 'them' }} under “Hidden”.
      </p>
      <p v-else-if="hiddenCount > 0" class="faint small hidden-note" data-testid="overview-hidden">
        {{ hiddenCount }} hidden {{ hiddenCount === 1 ? 'stack is' : 'stacks are' }} not shown; the sidebar lists them under “Hidden”.
      </p>
      <OpenersDialog :open="managing" :openers="hub.openers ?? []" @close="managing = false" />
    </template>
    <AddStackDialog :open="addOne" @close="addOne = false" />
  </div>
</template>

<style scoped>
.welcome {
  max-width: 820px;
  margin: 24px auto;
}
.welcome h1 {
  font-size: 18px;
  margin: 0;
}
.alt {
  margin-top: 16px;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.overview-head {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 16px;
}
.title {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 4px 12px;
}
.title h1 {
  margin: 0;
  font-size: 20px;
}
.counts {
  font-size: 13px;
}
.attn {
  color: var(--danger-text);
}
.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}
.filters {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 12px;
  margin-bottom: 14px;
}
.search {
  position: relative;
  flex: 1 1 240px;
  max-width: 420px;
  display: flex;
  align-items: center;
}
.search .muted {
  position: absolute;
  left: 9px;
  pointer-events: none;
}
.search .input {
  width: 100%;
  padding-left: 28px;
}
.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.empty {
  margin: 24px 0;
}
.link {
  border: 0;
  background: none;
  padding: 0;
  margin-left: 6px;
  color: var(--accent-text);
  cursor: pointer;
  text-decoration: underline;
  font: inherit;
}
.tiles {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(min(100%, 290px), 1fr));
  gap: 14px;
}
.hidden-note {
  margin-top: 14px;
}
.small {
  font-size: 12px;
}
</style>
