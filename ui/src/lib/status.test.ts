import { describe, expect, it } from 'vitest';
import type { Candidate, ContainerView, ServiceView, Status } from '@/api/types';
import { candidateLook, candidateSummary, containerBucket, containerLook, containerRefuses,kindIcon, kindLabel, parseComposeMember, rebuildLook, rebuildSummary, statusBucket, statusLook } from './status';

const cand = (state: string): Candidate => ({ state, build_id: null, since: 0, reason: null });
const svc = (kind: string): ServiceView => ({ kind }) as unknown as ServiceView;
const ctr = (state: string, health = '', exit_code: number | null = null): ContainerView => ({ state, health, exit_code }) as unknown as ContainerView;

describe('statusLook', () => {
  it('names each state', () => {
    expect(statusLook({ state: 'healthy' })).toMatchObject({ label: 'Healthy', tone: 'ok' });
    expect(statusLook({ state: 'building', build_id: null })).toMatchObject({ tone: 'accent', busy: true });
    expect(statusLook({ state: 'failed', reason: 'exit 1' })).toMatchObject({ tone: 'danger', detail: 'exit 1' });
    expect(statusLook({ state: 'completed', exit_code: 0 }).label).toBe('Completed');
    expect(statusLook({ state: 'exited', exit_code: 3 })).toMatchObject({ label: 'Exited (3)', tone: 'danger' });
  });

  it('shows a state of a newer daemon by its name', () => {
    const s = { state: 'hibernating' } as unknown as Status;
    expect(statusLook(s)).toMatchObject({ label: 'Hibernating', tone: 'neutral' });
  });

  it('buckets for the summary', () => {
    expect(statusBucket({ state: 'running' })).toBe('healthy');
    expect(statusBucket({ state: 'unhealthy', reason: '' })).toBe('failed');
    expect(statusBucket({ state: 'exited', exit_code: 0 })).toBe('idle');
    expect(statusBucket({ state: 'exited', exit_code: 1 })).toBe('failed');
  });
});

describe('candidates', () => {
  it('looks up known states', () => {
    expect(candidateLook(cand('held'))).toMatchObject({ label: 'Held', tone: 'warn', busy: false });
    expect(candidateSummary(cand('failed'))).toBe('build failed');
  });

  it('does not crash on a state it does not know (H-11)', () => {
    expect(candidateLook(cand('verifying'))).toMatchObject({ label: 'Verifying', tone: 'neutral' });
    expect(candidateSummary(cand('verifying'))).toBe('verifying');
    // Not a key of Object.prototype either.
    expect(candidateLook(cand('constructor')).label).toBe('Constructor');
  });
});

describe('kinds', () => {
  it('labels known kinds and passes others through', () => {
    expect(kindLabel(svc('devserver'))).toBe('dev server');
    expect(kindIcon(svc('static'))).toBe('file');
    expect(kindLabel(svc('sidecar'))).toBe('sidecar');
    expect(kindIcon(svc('sidecar'))).toBe('server');
  });
});

describe('containers', () => {
  it('reads Docker state and health', () => {
    expect(containerLook(ctr('running', 'healthy')).label).toBe('Healthy');
    expect(containerLook(ctr('running', 'starting')).busy).toBe(true);
    expect(containerLook(ctr('exited', '', 137)).label).toBe('Stopped');
    expect(containerLook(ctr('exited', '', 2))).toMatchObject({ tone: 'danger', detail: 'exit code 2' });
    expect(containerBucket(ctr('restarting'))).toBe('starting');
    expect(containerLook(ctr('')).label).toBe('Unknown');
  });

  it('knows which containers only stop', () => {
    const c = (managed?: boolean) => ({ name: 'shop-blobs', managed }) as unknown as ContainerView;
    expect(containerRefuses(null)).toBeNull();
    expect(containerRefuses(c(true))).toBeNull();
    expect(containerRefuses(c(undefined))).toBeNull();
    expect(containerRefuses(c(false))).toContain('shop-blobs');
  });

  it('parses compose members', () => {
    expect(parseComposeMember('compose.infra.db')).toEqual(['infra', 'db']);
    expect(parseComposeMember('compose.infra.db.replica')).toEqual(['infra', 'db.replica']);
    expect(parseComposeMember('api')).toBeNull();
  });
});

describe('rebuild (ADR-0042)', () => {
  const r = (state: string, ms?: number) => ({ state, since: 0, ms, tool: 'angular' });
  it('says how a dev server rebuilds', () => {
    expect(rebuildSummary(r('rebuilding'))).toBe('rebuilding');
    expect(rebuildSummary(r('ok', 11934))).toBe('rebuilt in 11.9 s');
    expect(rebuildSummary(r('ok', 412))).toBe('rebuilt in 412 ms');
    expect(rebuildSummary(r('ok'))).toBe('rebuilt');
    expect(rebuildSummary(r('failed', 523))).toBe('rebuild failed in 523 ms');
  });
  it('looks like a build', () => {
    expect(rebuildLook(r('rebuilding'))).toMatchObject({ tone: 'accent', busy: true });
    expect(rebuildLook(r('ok'))).toMatchObject({ tone: 'ok', busy: false });
    expect(rebuildLook(r('failed'))).toMatchObject({ tone: 'danger', label: 'Rebuild failed' });
    expect(rebuildLook(r('later'))).toMatchObject({ tone: 'neutral' });
  });
});
