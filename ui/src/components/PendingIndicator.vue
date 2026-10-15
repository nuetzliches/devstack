<script setup lang="ts">
// "N changes not live yet" in the top bar; opens a list of the candidates.
import { computed, ref } from 'vue';
import { useRouter } from 'vue-router';
import { ago, shortBuild, useNow } from '@/lib/format';
import { usePopover } from '@/lib/popover';
import { candidateLook, candidateSummary } from '@/lib/status';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const stack = useStack();
const router = useRouter();
const now = useNow();
const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const { open, toggle, close } = usePopover(root, trigger);

const items = computed(() => stack.pending);
const worst = computed(() => {
  const states = items.value.map((s) => s.candidate!.state);
  if (states.includes('failed')) return 'danger';
  if (states.includes('held')) return 'warn';
  return 'accent';
});
const anyBusy = computed(() => items.value.some((s) => candidateLook(s.candidate!).busy));

function go(name: string) {
  close();
  void router.push({ name: 'service', params: { stack: stack.id ?? '', name } });
}
</script>

<template>
  <div v-if="items.length" ref="root" class="pending">
    <button
      ref="trigger"
      type="button"
      class="pbtn"
      :class="`tone-${worst}`"
      :aria-expanded="open"
      aria-haspopup="true"
      :aria-label="`${items.length} change${items.length === 1 ? '' : 's'} not live yet`"
      data-testid="pending-indicator"
      @click="toggle"
    >
      <span v-if="anyBusy" class="spinner" aria-hidden="true" />
      <Icon v-else :name="worst === 'danger' ? 'xCircle' : 'hourglass'" :size="14" />
      <span class="count">{{ items.length }}</span>
      <span class="hide-sm">not live yet</span>
    </button>
    <Transition name="drop">
      <div v-if="open" class="menu" data-testid="pending-menu">
        <div class="menu-head">Not yet active</div>
        <button v-for="s in items" :key="s.name" type="button" class="item" :class="`tone-${candidateLook(s.candidate!).tone}`" @click="go(s.name)">
          <span class="i-icon">
            <span v-if="candidateLook(s.candidate!).busy" class="spinner sm" />
            <Icon v-else :name="candidateLook(s.candidate!).icon" :size="14" />
          </span>
          <span class="grow">
            <span class="i-name">{{ s.name }}</span>
            <span class="i-sub">{{ candidateSummary(s.candidate!) }}<template v-if="s.candidate!.build_id"> · {{ shortBuild(s.candidate!.build_id) }}</template></span>
          </span>
          <span class="i-ago">{{ ago(s.candidate!.since, now) }}</span>
        </button>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.pending {
  position: relative;
}
.pbtn {
  --tone: var(--accent);
  --tone-text: var(--accent-text);
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 30px;
  padding: 0 10px;
  border-radius: 999px;
  border: 1px solid color-mix(in srgb, var(--tone) 40%, transparent);
  background: color-mix(in srgb, var(--tone) 13%, transparent);
  color: var(--tone-text);
  font-weight: 600;
  font-size: 12.5px;
  cursor: pointer;
  white-space: nowrap;
}
.pbtn:hover {
  background: color-mix(in srgb, var(--tone) 20%, transparent);
}
.tone-warn {
  --tone: var(--warn);
  --tone-text: var(--warn-text);
}
.tone-danger {
  --tone: var(--danger);
  --tone-text: var(--danger-text);
}
.tone-info {
  --tone: var(--info);
  --tone-text: var(--info-text);
}
.tone-neutral {
  --tone: var(--text-faint);
  --tone-text: var(--text-muted);
}
.count {
  font-variant-numeric: tabular-nums;
}
.spinner {
  width: 12px;
  height: 12px;
}
.menu {
  position: absolute;
  right: 0;
  top: calc(100% + 6px);
  z-index: 40;
  width: min(320px, calc(100vw - 24px));
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-lg);
  padding: 6px;
}
.menu-head {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
  padding: 6px 8px 4px;
}
.item {
  --tone: var(--accent);
  --tone-text: var(--accent-text);
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  border: 0;
  background: transparent;
  padding: 8px;
  border-radius: 8px;
  cursor: pointer;
  text-align: left;
}
.item:hover,
.item:focus-visible {
  background: var(--surface-hover);
}
.i-icon {
  width: 26px;
  height: 26px;
  border-radius: 7px;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--tone) 15%, transparent);
  color: var(--tone-text);
  flex: none;
}
.i-name {
  display: block;
  font-weight: 600;
}
.i-sub {
  display: block;
  font-size: 12px;
  color: var(--tone-text);
}
.i-ago {
  font-size: 11.5px;
  color: var(--text-muted);
  white-space: nowrap;
}
.drop-enter-active,
.drop-leave-active {
  transition:
    opacity 0.12s,
    transform 0.12s var(--ease);
}
.drop-enter-from,
.drop-leave-to {
  opacity: 0;
  transform: translateY(-4px);
}
@media (max-width: 640px) {
  .hide-sm {
    display: none;
  }
}
</style>
