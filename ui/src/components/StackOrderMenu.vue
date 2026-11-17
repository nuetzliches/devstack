<script setup lang="ts">
// The menu that orders the stacks (by name, last opened, last edited): the
// sidebar's and the start page's, one choice for both (lib/stackOrder.ts).
// The page draws the button that opens it, in its own look, from the slot.
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import { useOrderChoice, type StackOrder } from '@/lib/stackOrder';
import Icon from './icons/Icon.vue';

const props = defineProps<{ menuTestId: string }>();

const choice = useOrderChoice();
const options: { value: StackOrder; label: string; title: string }[] = [
  { value: 'name', label: 'By name', title: 'by name' },
  { value: 'opened', label: 'Last opened first', title: 'last opened first' },
  { value: 'edited', label: 'Last edited first', title: 'last edited first' },
];
/** For the button's tooltip. */
const title = computed(() => `Order: ${options.find((o) => o.value === choice.order.value)?.title ?? 'by name'}`);

const open = ref(false);
const menuEl = ref<HTMLElement | null>(null);
const button = ref<HTMLElement | null>(null);
const pos = ref({ x: 0, y: 0 });
const setButton = (el: unknown) => {
  button.value = (el as HTMLElement | null) ?? null;
};

function toggle() {
  if (open.value) return close();
  const r = button.value!.getBoundingClientRect();
  pos.value = { x: Math.max(4, r.right - 180), y: r.bottom + 2 };
  open.value = true;
  void nextTick(() => menuEl.value?.querySelector<HTMLButtonElement>('[aria-checked="true"]')?.focus());
}
function close() {
  open.value = false;
}
function pick(o: StackOrder) {
  choice.choose(o);
  close();
}
function onDown(e: PointerEvent) {
  if (menuEl.value?.contains(e.target as Node) || button.value?.contains(e.target as Node)) return;
  close();
}
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    e.preventDefault();
    close();
    button.value?.focus();
  }
}
watch(open, (now) => {
  const add = now ? 'addEventListener' : 'removeEventListener';
  document[add]('pointerdown', onDown as EventListener, true);
  document[add]('keydown', onKey as EventListener);
  window[add]('blur', close);
});
onBeforeUnmount(close);
</script>

<template>
  <slot :toggle="toggle" :open="open" :set-button="setButton" :title="title" />
  <Teleport to="body">
    <div
      v-if="open"
      ref="menuEl"
      class="order-menu"
      role="menu"
      aria-label="Order of the stacks"
      :style="{ left: `${pos.x}px`, top: `${pos.y}px` }"
      :data-testid="props.menuTestId"
    >
      <button
        v-for="o in options"
        :key="o.value"
        type="button"
        class="opt"
        role="menuitemradio"
        :aria-checked="choice.order.value === o.value"
        :data-order="o.value"
        @click="pick(o.value)"
      >
        <Icon v-if="choice.order.value === o.value" name="check" :size="14" />
        <span v-else class="check-space" aria-hidden="true" />
        {{ o.label }}
      </button>
    </div>
  </Teleport>
</template>

<style scoped>
.order-menu {
  position: fixed;
  z-index: 60;
  min-width: 180px;
  padding: 4px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--bg-elev);
  box-shadow: var(--shadow-lg, 0 8px 24px rgb(0 0 0 / 0.25));
  display: flex;
  flex-direction: column;
}
.opt {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
}
.opt:hover,
.opt:focus-visible {
  background: var(--surface-hover);
  outline: none;
}
.check-space {
  width: 14px;
  flex: none;
}
</style>
