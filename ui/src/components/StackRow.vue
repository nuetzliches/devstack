<script setup lang="ts">
// One stack as a row of the start page's list: the same figures as the tile
// (lib/stackFigures.ts), one per cell, and the favourite programs at the end.
// The name is a link; a click anywhere else on the row opens the stack too,
// the star and the programs excepted.
import { computed } from 'vue';
import { useRouter } from 'vue-router';
import type { StackEntry } from '@/api/types';
import { commitAge, duration, useNow } from '@/lib/format';
import { stackFigures } from '@/lib/stackFigures';
import { stackPage } from '@/lib/stackRoutes';
import { useHub } from '@/stores/hub';
import Icon from './icons/Icon.vue';
import StatusPill from './StatusPill.vue';
import FavoriteOpeners from './FavoriteOpeners.vue';

const props = defineProps<{ stack: StackEntry; favourite: boolean }>();
const emit = defineEmits<{ toggleFavourite: [] }>();

const hub = useHub();
const router = useRouter();
const now = useNow();

const figures = computed(() => stackFigures(props.stack, hub));
const upFor = computed(() => (figures.value.upSince !== null ? duration(now.value - figures.value.upSince) : null));
const committed = computed(() => commitAge(figures.value.committedAt, now.value));
/** What needs attention, one line each: the cell's tooltip. */
const notes = computed(() => figures.value.items.map((i) => `${i.lane ? `${i.lane}: ` : ''}${i.text}`).join('\n'));
const worst = computed(() => (figures.value.items.some((i) => i.level === 'error') ? 'error' : 'warn'));

function open(e: Event) {
  if ((e.target as Element).closest('a, button')) return;
  void router.push(stackPage(props.stack.id));
}
</script>

<template>
  <tr class="stack-row" tabindex="0" data-testid="stack-row" :data-id="stack.id" @click="open" @keydown.enter="open">
    <td class="star-cell">
      <button
        type="button"
        class="star"
        :class="{ on: favourite }"
        :aria-pressed="favourite"
        :title="favourite ? 'Remove from favourites' : 'Add to favourites'"
        :aria-label="favourite ? `Remove ${stack.name} from favourites` : `Add ${stack.name} to favourites`"
        data-testid="tile-star"
        @click="emit('toggleFavourite')"
      >
        <Icon name="star" :size="14" />
      </button>
    </td>
    <td class="name-cell">
      <RouterLink :to="stackPage(stack.id)" class="name" data-testid="tile-link">{{ stack.name }}</RouterLink>
      <Icon v-if="stack.no_file" name="alert" :size="13" class="warn-icon" title="Its stack file is missing" />
      <div class="path mono faint" :title="stack.repo">{{ stack.repo }}</div>
    </td>
    <td><StatusPill :tone="figures.look.tone" :label="figures.look.label" :busy="figures.look.busy" size="sm" data-testid="tile-state" /></td>
    <td class="num" data-testid="tile-services">
      <template v-if="figures.services">{{ figures.services.ready }}<span class="faint">/{{ figures.services.total }}</span></template>
      <span v-else class="faint">—</span>
    </td>
    <td class="num" data-testid="tile-lanes">{{ figures.lanes ?? '—' }}</td>
    <td class="num hide-sm" data-testid="tile-up">{{ upFor ?? '—' }}</td>
    <td class="num" data-testid="tile-commit">{{ committed ?? '—' }}</td>
    <td class="num attention" data-testid="row-attention" :title="notes || undefined">
      <span v-if="figures.items.length" class="count" :class="worst"><Icon :name="worst === 'warn' ? 'pause' : 'xCircle'" :size="13" /> {{ figures.items.length }}</span>
      <span v-else class="faint">—</span>
    </td>
    <td class="programs"><div class="progs"><FavoriteOpeners v-if="hub.reachable" :stack-id="stack.id" :path="stack.repo" /></div></td>
  </tr>
</template>

<style scoped>
/* Not `row`: that is a global flex helper, and a flex `tr` leaves the table's columns. */
.stack-row {
  cursor: pointer;
}
.stack-row:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: -2px;
}
.star-cell {
  width: 1%;
  padding-right: 0;
}
.star {
  display: grid;
  place-items: center;
  width: 22px;
  height: 22px;
  padding: 0;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--text-faint);
  cursor: pointer;
}
.star:hover {
  background: var(--surface-hover);
  color: var(--text);
}
.star.on {
  color: var(--warn);
}
.star.on :deep(svg) {
  fill: currentColor;
}
.name-cell {
  min-width: 0;
  max-width: 320px;
}
.name {
  color: var(--text);
  font-weight: 600;
  text-decoration: none;
}
.name:hover {
  text-decoration: underline;
}
.warn-icon {
  margin-left: 6px;
  vertical-align: -2px;
  color: var(--warn-text);
}
.path {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 12px;
}
.count {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-weight: 600;
}
.count.error {
  color: var(--danger-text);
}
.count.warn {
  color: var(--warn-text);
}
.programs {
  width: 1%;
}
.progs {
  display: flex;
  justify-content: flex-end;
  gap: 6px;
}
@media (max-width: 760px) {
  .hide-sm {
    display: none;
  }
}
</style>
