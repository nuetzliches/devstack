// A filter on an upstream tool's result as the form edits it, and back
// (ADR-0069, addendum 2026-11-10).
import type { UpstreamFilter } from '@/api/types';
import { line, words } from '@/lib/commandLine';

export type FilterForm = {
  upstream: string;
  tool: string;
  /** Paths apart by commas. */
  paths: string;
  command: string;
  /** Empty: no write-back. */
  writeTool: string;
  /** `name=value`, one per line. */
  writeArgs: string;
};

export function filterForm(f?: UpstreamFilter, upstream = ''): FilterForm {
  return {
    upstream: f?.upstream ?? upstream,
    tool: f?.tool ?? '',
    paths: (f?.paths ?? []).join(', '),
    command: line(f?.command ?? []),
    writeTool: f?.write?.tool ?? '',
    writeArgs: Object.entries(f?.write?.arguments ?? {})
      .map(([k, v]) => `${k}=${v}`)
      .join('\n'),
  };
}

export function filterFrom(f: FilterForm): UpstreamFilter {
  const out: UpstreamFilter = {
    upstream: f.upstream,
    tool: f.tool.trim(),
    paths: f.paths
      .split(',')
      .map((p) => p.trim())
      .filter(Boolean),
    command: words(f.command.trim()),
  };
  if (f.writeTool.trim()) {
    const args = Object.fromEntries(
      f.writeArgs
        .split('\n')
        .map((l) => l.trim())
        .filter((l) => l.includes('='))
        .map((l) => [l.slice(0, l.indexOf('=')).trim(), l.slice(l.indexOf('=') + 1).trim()]),
    );
    out.write = { tool: f.writeTool.trim(), arguments: args };
  }
  return out;
}

/** A Linear issue's description through `veilq mark`, stored back marked. */
export function veilqLinear(upstream: string): FilterForm {
  return {
    upstream,
    tool: 'get_issue',
    paths: 'description',
    command: 'veilq mark -',
    writeTool: 'save_issue',
    writeArgs: 'id=$args.id\ndescription=$write',
  };
}

/** One line for the list: what runs on what. */
export function filterSummary(f: UpstreamFilter): string {
  const w = f.write ? ` · stored back with ${f.write.tool}` : '';
  return `${f.tool}: ${f.paths.join(', ')} through ${line(f.command)}${w}`;
}
