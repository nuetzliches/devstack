import { onBeforeUnmount, ref, watch, type Ref } from 'vue';

/** Open/close state for a dropdown: closes on outside click and Escape, returns focus to the trigger. */
export function usePopover(root: Ref<HTMLElement | null>, trigger: Ref<HTMLElement | null>) {
  const open = ref(false);

  function onDown(e: PointerEvent) {
    if (root.value && !root.value.contains(e.target as Node)) open.value = false;
  }
  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape' && open.value) {
      e.preventDefault();
      open.value = false;
      trigger.value?.focus();
    }
  }

  watch(open, (o) => {
    if (o) {
      document.addEventListener('pointerdown', onDown, true);
      document.addEventListener('keydown', onKey);
    } else {
      document.removeEventListener('pointerdown', onDown, true);
      document.removeEventListener('keydown', onKey);
    }
  });

  onBeforeUnmount(() => {
    document.removeEventListener('pointerdown', onDown, true);
    document.removeEventListener('keydown', onKey);
  });

  return { open, toggle: () => (open.value = !open.value), close: () => (open.value = false) };
}
