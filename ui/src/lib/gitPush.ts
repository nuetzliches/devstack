// Push the branch of a tree to its own upstream, after asking (ADR-0117): the
// hub runs `git push` for the folder named, when its branch is ahead and not
// behind. One call for the badge on the start page, the lane's header and the
// command palette, so the question and the toasts read the same everywhere.
import { paths } from '@/api';
import { act } from '@/lib/actions';

/** What a tree's branch is, against its upstream. */
export type TreeSync = { branch: string; upstream: string; ahead: number; behind: number };

export const plural = (n: number) => `${n} commit${n === 1 ? '' : 's'}`;

/** Ahead and not behind: the only state the page pushes from. */
export const pushable = (s: TreeSync) => s.ahead > 0 && s.behind === 0;

/**
 * Ask, then push the tree at `path` of stack `stackId`; whether it was pushed.
 * `stackName`: where more than one stack is in view (the start page), the
 * question and the answer say which.
 */
export async function pushTree(stackId: string, path: string, s: TreeSync, stackName?: string): Promise<boolean> {
  const of = stackName ? `${s.branch} (${stackName})` : s.branch;
  const r = await act({
    label: `Push ${of}`,
    path: paths.push(stackId),
    body: { path },
    confirmText: `Push ${plural(s.ahead)} of ${of} to ${s.upstream}?`,
    confirmLabel: 'Push',
    danger: false,
    success: `Pushed ${of} to ${s.upstream}`,
  });
  return r.ok;
}
