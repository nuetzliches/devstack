// A sketch of the board's display protocol (idea #378), for the mock only.
// A tile's content is data in a small vocabulary of elements that the
// dashboard renders; a source that speaks it is shown as it is, any other
// is mapped by the developer's agent. No element carries code.

export type Tone = 'ok' | 'warn' | 'err' | 'info' | 'idle';

export type BoardElement =
  | { type: 'list'; items: { title: string; url?: string; detail?: string; state?: Tone; badge?: string; time?: number }[]; empty?: string }
  | { type: 'status'; items: { label: string; state: Tone; detail?: string; url?: string }[] }
  | { type: 'number'; value: number; unit?: string; label?: string; delta?: number; trend?: number[]; state?: Tone }
  | { type: 'text'; text: string }
  | { type: 'links'; items: { label: string; url: string; detail?: string }[] }
  | { type: 'table'; columns: string[]; rows: (string | number)[][] }
  | { type: 'timeline'; items: { time: number; title: string; detail?: string; state?: Tone }[] };

/** Where a tile comes from: the developer's own file, or a pool (mandatory or accepted). */
export type TileOrigin = { kind: 'mine' } | { kind: 'pool'; pool: string; mandatory: boolean };

export type BoardTile = {
  id: string;
  title: string;
  icon?: string;
  /** Columns of a three-column grid. */
  width: 1 | 2 | 3;
  origin: TileOrigin;
  /** What feeds it, as the board file names it (`mcp: …`, `command: …`, `http: …`, `link`). */
  source: string;
  every?: string;
  refreshed: number;
  element: BoardElement;
  error?: string;
};

export type Board = { version: 1; file: string; tiles: BoardTile[] };

export type AttentionOrigin = 'devstack' | 'tile' | 'push' | 'pool';

/** One thing that needs the developer; devstack keeps whether it was seen, not the alarm itself. */
export type AttentionItem = {
  id: string;
  origin: AttentionOrigin;
  /** Who said so: a stack and lane, a tile, a pushing tool, a pool. */
  from: string;
  title: string;
  detail?: string;
  state: Tone;
  time: number;
  mandatory?: boolean;
  action?: { label: string; kind: 'promote' | 'open' | 'link'; stack?: string; url?: string };
  seen: boolean;
  snoozed_until?: number;
};
