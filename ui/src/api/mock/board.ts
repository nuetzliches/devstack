// The mock's board and attention inbox (idea #378): what a developer's agent
// might have built, plus two pool tiles. Changes over time so the page shows
// a check turning red, a push arriving and a list gaining an entry.
import type { AttentionItem, Board, BoardTile } from '../board';

const now = () => Math.floor(Date.now() / 1000);
const t0 = now();

const tiles = (): BoardTile[] => {
  const age = now() - t0;
  const stagingDown = age > 20 && age < 80;
  return [
    {
      id: 'health',
      title: 'Health',
      icon: 'activity',
      width: 1,
      origin: { kind: 'mine' },
      source: 'http: https://status.example.com/api/v1/endpoints/statuses',
      every: '30s',
      refreshed: now() - 4,
      element: {
        type: 'status',
        items: [
          { label: 'api (production)', state: 'ok', detail: '99.98 % · 142 ms' },
          { label: 'web (production)', state: 'ok', detail: '100 % · 61 ms' },
          { label: 'api (staging)', state: stagingDown ? 'err' : 'ok', detail: stagingDown ? 'HTTP 502 since 12:41' : '99.7 % · 188 ms' },
          { label: 'worker queue', state: 'warn', detail: '1 240 waiting' },
        ],
      },
    },
    {
      id: 'issues',
      title: 'My issues, this cycle',
      icon: 'list',
      width: 2,
      origin: { kind: 'mine' },
      source: 'mcp: tracker__list_issues { assignee: me, cycle: current }',
      every: '120s',
      refreshed: now() - 37,
      element: {
        type: 'list',
        items: [
          { title: 'Checkout keeps the old address after a change', badge: 'APP-412', state: 'err', detail: 'In progress · high', url: 'https://tracker.example.com/APP-412' },
          { title: 'Invoice export as CSV', badge: 'APP-398', state: 'info', detail: 'In review', url: 'https://tracker.example.com/APP-398' },
          ...(age > 45 ? [{ title: 'Search ignores umlauts', badge: 'APP-431', state: 'warn' as const, detail: 'Todo · new, assigned 12:52', url: 'https://tracker.example.com/APP-431' }] : []),
          { title: 'Remove the old login page', badge: 'APP-377', state: 'idle', detail: 'Todo', url: 'https://tracker.example.com/APP-377' },
        ],
      },
    },
    {
      id: 'reviews',
      title: 'Pull requests waiting for me',
      icon: 'check',
      width: 2,
      origin: { kind: 'mine' },
      source: 'command: gh search prs --review-requested=@me --state=open --json …',
      every: '60s',
      refreshed: now() - 12,
      element: {
        type: 'table',
        columns: ['PR', 'Title', 'Author', 'Checks', 'Age'],
        rows: [
          ['#1284', 'feat(api): paging for orders', 'ana', 'green', '2 h'],
          ['#1279', 'fix(web): cart total rounding', 'tom', 'running', '5 h'],
          ['#1266', 'chore: bump the ORM', 'renovate', 'red', '1 d'],
        ],
      },
    },
    {
      id: 'mail',
      title: 'Latest messages',
      icon: 'bell',
      width: 1,
      origin: { kind: 'mine' },
      source: 'mcp: mail__list_messages { folder: inbox, top: 5 }',
      every: '120s',
      refreshed: now() - 50,
      element: {
        type: 'list',
        items: [
          { title: 'Re: release on Thursday?', detail: 'Ana', state: 'info', time: now() - 600 },
          { title: 'Your build minutes this month', detail: 'CI service', time: now() - 3600 },
          { title: 'Lunch?', detail: 'Tom', time: now() - 7200 },
        ],
      },
    },
    {
      id: 'deploys',
      title: 'Deploys today',
      icon: 'zap',
      width: 1,
      origin: { kind: 'pool', pool: 'default', mandatory: false },
      source: 'mcp: ci__deployments { since: today }',
      every: '60s',
      refreshed: now() - 20,
      element: {
        type: 'timeline',
        items: [
          { time: now() - 1500, title: 'api 2.14.1 → production', state: 'ok' },
          { time: now() - 5400, title: 'web 3.2.0 → staging', state: 'ok' },
          { time: now() - 9000, title: 'worker 1.9.0 → production', state: 'err', detail: 'rolled back' },
        ],
      },
    },
    {
      id: 'errors',
      title: 'Errors in production, 24 h',
      icon: 'alert',
      width: 1,
      origin: { kind: 'mine' },
      source: 'http: https://errors.example.com/api/stats?project=api',
      every: '300s',
      refreshed: now() - 90,
      element: { type: 'number', value: 37, label: 'events, 4 new issues', delta: 12, trend: [8, 11, 9, 14, 10, 22, 25, 37], state: 'warn' },
    },
    {
      id: 'team',
      title: 'Team',
      icon: 'link',
      width: 1,
      origin: { kind: 'pool', pool: 'default', mandatory: true },
      source: 'link',
      refreshed: now() - 3600,
      element: {
        type: 'links',
        items: [
          { label: 'Release calendar', url: 'https://wiki.example.com/releases', detail: 'next: Thursday' },
          { label: 'On call this week', url: 'https://wiki.example.com/oncall', detail: 'Ana' },
          { label: 'Runbooks', url: 'https://wiki.example.com/runbooks' },
        ],
      },
    },
    {
      id: 'notes',
      title: 'Today',
      icon: 'file',
      width: 1,
      origin: { kind: 'mine' },
      source: 'command: cat ~/notes/today.md',
      refreshed: now() - 300,
      element: { type: 'text', text: 'Finish APP-412 before the release.\nReview #1284 (Ana asked).\nAsk Tom about the ORM bump.' },
    },
  ];
};

export function mockBoard(): Board {
  return { version: 1, file: '~/.config/devstack/board.toml', tiles: tiles() };
}

const items: AttentionItem[] = [
  {
    id: 'a1',
    origin: 'devstack',
    from: 'shop · feat-paging',
    title: 'A candidate of api waits: a schema change',
    detail: 'Built 4 min ago; the old build answers until you promote it.',
    state: 'warn',
    time: t0 - 240,
    action: { label: 'Promote', kind: 'promote', stack: 'shop-3f9a1c' },
    seen: false,
  },
  {
    id: 'a2',
    origin: 'devstack',
    from: 'crm · main',
    title: 'web failed to build',
    detail: "src/app/orders.ts(41,7): error TS2322: Type 'string' is not assignable to type 'number'.",
    state: 'err',
    time: t0 - 60,
    action: { label: 'Open', kind: 'open', stack: 'crm-81c0d2' },
    seen: false,
  },
];

const later: { at: number; item: AttentionItem }[] = [
  {
    at: 22,
    item: {
      id: 'a3',
      origin: 'tile',
      from: 'Health',
      title: 'api (staging) is down',
      detail: 'HTTP 502 since 12:41',
      state: 'err',
      time: 0,
      action: { label: 'Status page', kind: 'link', url: 'https://status.example.com' },
      seen: false,
    },
  },
  {
    at: 35,
    item: {
      id: 'a4',
      origin: 'pool',
      from: 'pool default',
      title: 'Release freeze from 16:00',
      detail: 'Only fixes to the release branch until Thursday.',
      state: 'info',
      time: 0,
      mandatory: true,
      seen: false,
    },
  },
  {
    at: 50,
    item: {
      id: 'a5',
      origin: 'push',
      from: 'ci (devstack notify)',
      title: 'Nightly e2e: 3 tests failed',
      detail: 'checkout.spec.ts, search.spec.ts, login.spec.ts',
      state: 'err',
      time: 0,
      action: { label: 'Run', kind: 'link', url: 'https://ci.example.com/runs/9182' },
      seen: false,
    },
  },
];

export function mockAttention(): AttentionItem[] {
  const age = now() - t0;
  for (const l of later.filter((l) => l.at <= age && !items.some((i) => i.id === l.item.id))) items.unshift({ ...l.item, time: now() });
  return items.filter((i) => !i.snoozed_until || i.snoozed_until <= now());
}

export function mockAttentionAct(id: string, act: string): boolean {
  const i = items.find((x) => x.id === id);
  if (!i) return false;
  if (act === 'seen') i.seen = true;
  else if (act === 'snooze') i.snoozed_until = now() + 3600;
  else if (act === 'done') items.splice(items.indexOf(i), 1);
  else return false;
  return true;
}
