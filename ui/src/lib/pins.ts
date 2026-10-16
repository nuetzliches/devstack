// Pinned services and containers, per stack, in this browser only: they show
// first in the overview. A member is keyed as the stack file names it in a
// group: `api`, `compose.app.db`.
import { ref } from 'vue';
import { persist } from './persist';

const pins = ref<Record<string, string[]>>({});

function load(stack: string): string[] {
  if (!(stack in pins.value)) pins.value[stack] = persist.get<string[]>(`pins.${stack}`) ?? [];
  return pins.value[stack];
}

export function usePins() {
  return {
    list: (stack: string | null) => (stack ? load(stack) : []),
    has: (stack: string | null, key: string) => !!stack && load(stack).includes(key),
    toggle(stack: string | null, key: string) {
      if (!stack) return;
      const cur = load(stack);
      const next = cur.includes(key) ? cur.filter((k) => k !== key) : [...cur, key];
      pins.value[stack] = next;
      persist.set(`pins.${stack}`, next);
    },
  };
}
