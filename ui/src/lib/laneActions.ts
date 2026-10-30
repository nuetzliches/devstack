import type { LaneView } from '@/api/types';
import type { IconName } from '@/components/icons/icons';
import { laneLabel, MAIN } from './lanes';

/** One entry of a menu: what it says, and whether it may be chosen now. */
export type MenuItem = { key: string; label: string; hint?: string; icon?: IconName; danger?: boolean; disabled?: boolean; title?: string };

export type LaneAction = 'restart' | 'rebuild' | 'cold' | 'start' | 'stop' | 'reset' | 'remove';

/**
 * Why an action of a lane has no meaning for the main working tree (the lane
 * `main`, ADR-0080): shown on the item, which stays in the menu, disabled
 * (ADR-0081). The daemon keeps the main tree as its own tree: it runs in
 * place, on this machine, with the daemon's profile.
 */
export const NOT_FOR_MAIN = {
  cold: 'The main working tree has no cold restart: the daemon throws away and seeds again a lane’s data only',
  reset: 'The main working tree’s data is not discarded from here: the daemon does that for a lane’s only',
  remove: 'The main working tree is the repository itself: only a linked worktree is removed',
  move: 'The main working tree runs in place, on this machine: a worktree’s lane moves to a node',
  share: 'Only a lane on a node of the pool is shared: the main working tree runs on this machine',
  pool: 'The main working tree runs on this machine, on its own ports: a worktree’s lane starts in the pool',
  profile: 'The main working tree runs with the profile its daemon started with (devstack up --profile in it)',
} as const;

/**
 * A lane's actions, in one place for the header and the branch picker: the
 * main one (Restart when it is started, Start when not) and the others. The
 * three restarts: Restart (warm: from the artifacts there are, its data
 * kept), Rebuild (built again, its data kept), Cold (built again, its data
 * seeded again; asks). Discard data and Remove worktree only for a stopped
 * lane. Start runs it on this machine; the card below the header starts
 * it on the pool too. The main working tree has the same items; what has no
 * meaning for it is disabled with the reason (ADR-0081).
 */
export function laneActions(l: LaneView): { main: MenuItem & { key: LaneAction; icon: IconName }; items: (MenuItem & { key: LaneAction })[] } {
  const name = laneLabel(l);
  const isMain = l.name === MAIN;
  const busy = !!l.moving || !!l.removing;
  if (l.active !== false) {
    // The main working tree may stop while it serves the main ports: they stay its own.
    const servesMain = l.owns_main && !isMain;
    return {
      main: { key: 'restart', label: 'Restart', icon: 'restart', title: `Restart ${name}'s running services where it runs, from the builds there are for their inputs; its data stays`, disabled: busy },
      items: [
        { key: 'rebuild', label: 'Rebuild', icon: 'wrench', hint: 'data stays', title: `Build ${name}'s running services again, even for inputs built before, and restart them; its data stays`, disabled: busy },
        isMain
          ? { key: 'cold', label: 'Cold…', icon: 'database', hint: 'data seeded again', danger: true, title: NOT_FOR_MAIN.cold, disabled: true }
          : { key: 'cold', label: 'Cold…', icon: 'database', hint: 'data seeded again', danger: true, title: `Stop ${name}, throw its data away and start it again where it ran: built again, seeded again`, disabled: busy || servesMain },
        {
          key: 'stop',
          label: 'Stop',
          icon: 'stop',
          title: servesMain
            ? `${name} serves the main ports: give them to another lane first (Serve main ports there, or the stack page)`
            : `Stop ${name}: its services and containers stop; ${isMain ? 'the daemon and the other lanes keep running' : 'the worktree stays'}`,
          disabled: busy || servesMain,
        },
      ],
    };
  }
  return {
    main: { key: 'start', label: 'Start', icon: 'play', title: isMain ? `Start ${name} on this machine, on its own ports` : `Start ${name} on this machine (the card below starts it on the pool too)`, disabled: busy },
    items: [
      isMain
        ? { key: 'reset', label: 'Discard data…', icon: 'trash', danger: true, title: NOT_FOR_MAIN.reset, disabled: true }
        : { key: 'reset', label: 'Discard data…', icon: 'trash', danger: true, title: `Throw ${name}'s data away (its containers and volumes, here and on the nodes it ran on): it starts fresh`, disabled: busy },
      isMain
        ? { key: 'remove', label: 'Remove worktree…', icon: 'trash', danger: true, title: NOT_FOR_MAIN.remove, disabled: true }
        : { key: 'remove', label: 'Remove worktree…', icon: 'trash', danger: true, title: `Remove the worktree ${l.path} (git refuses one with changes; the branch stays)`, disabled: busy || l.owns_main },
    ],
  };
}
