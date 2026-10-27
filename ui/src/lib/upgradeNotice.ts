// Whether the open stack's "stack file behind" notice shows (UpgradeBanner).
// Hidden, it stays hidden until the next minor devstack. While it shows, it
// also says that the agent skills are from an older devstack: SkillsBanner
// leaves that case to it.
import { computed, ref } from 'vue';
import type { SkillsState } from '@/api/types';
import { persist } from '@/lib/persist';
import { useStack } from '@/stores/stack';

/** Bumped when the notice is hidden: `persist` itself is not reactive. */
const hiddenAt = ref(0);

/** `0.25.1` and `0.25.0-rc.1` are `0.25`: a patch release does not show it again. */
export function minor(version: string): string {
  return version.split(/[.-]/).slice(0, 2).join('.');
}

/** `ahead`: written by a newer devstack; `older`: by an older one; `file`: the stack file changed. */
export function skillsState(s: SkillsState | null): 'ahead' | 'older' | 'file' | null {
  if (!s || !s.outdated.length) return null;
  if (s.ahead) return 'ahead';
  return s.from === s.current ? 'file' : 'older';
}

export function useUpgradeNotice() {
  const stack = useStack();
  const key = () => `upgradeDismissed.${stack.id}`;

  const shown = computed(() => {
    void hiddenAt.value;
    const u = stack.upgrade;
    return !!stack.id && !!u?.behind && persist.get<string>(key()) !== minor(u.current);
  });

  function hide() {
    if (!stack.upgrade) return;
    persist.set(key(), minor(stack.upgrade.current));
    hiddenAt.value++;
  }

  return { shown, hide };
}
