import type { LaneView } from '@/api/types';
import type { IconName } from '@/components/icons/icons';
import { laneLabel } from './lanes';

/** One entry of a menu: what it says, and whether it may be chosen now. */
export type MenuItem = { key: string; label: string; hint?: string; icon?: IconName; danger?: boolean; disabled?: boolean; title?: string };

export type LaneAction = 'restart' | 'rebuild' | 'cold' | 'start' | 'stop' | 'reset' | 'remove';

/**
 * A lane's actions, in one place for the header and the branch picker: the
 * main one (Restart when it is started, Start when not) and the others. The
 * three restarts: Restart (warm: from the artifacts there are, its data
 * kept), Rebuild (built again, its data kept), Cold (built again, its data
 * seeded again; asks). Discard data and Remove worktree only for a stopped
 * lane. Start runs it on this machine; the card below the header starts
 * it on the pool too.
 */
export function laneActions(l: LaneView): { main: MenuItem & { key: LaneAction; icon: IconName }; items: (MenuItem & { key: LaneAction })[] } {
  const name = laneLabel(l);
  const busy = !!l.moving || !!l.removing;
  if (l.active !== false) {
    const servesMain = l.owns_main;
    return {
      main: { key: 'restart', label: 'Restart', icon: 'restart', title: `Restart ${name}'s running services where it runs, from the builds there are for their inputs; its data stays`, disabled: busy },
      items: [
        { key: 'rebuild', label: 'Rebuild', icon: 'wrench', hint: 'data stays', title: `Build ${name}'s running services again, even for inputs built before, and restart them; its data stays`, disabled: busy },
        { key: 'cold', label: 'Cold…', icon: 'database', hint: 'data seeded again', danger: true, title: `Stop ${name}, throw its data away and start it again where it ran: built again, seeded again`, disabled: busy || servesMain },
        {
          key: 'stop',
          label: 'Stop',
          icon: 'stop',
          title: servesMain ? `${name} serves the main ports: give them back first` : `Stop ${name}: its services and containers stop; the worktree stays`,
          disabled: busy || servesMain,
        },
      ],
    };
  }
  return {
    main: { key: 'start', label: 'Start', icon: 'play', title: `Start ${name} on this machine (the card below starts it on the pool too)`, disabled: busy },
    items: [
      { key: 'reset', label: 'Discard data…', icon: 'trash', danger: true, title: `Throw ${name}'s data away (its containers and volumes, here and on the nodes it ran on): it starts fresh`, disabled: busy },
      { key: 'remove', label: 'Remove worktree…', icon: 'trash', danger: true, title: `Remove the worktree ${l.path} (git refuses one with changes; the branch stays)`, disabled: busy || l.owns_main },
    ],
  };
}
