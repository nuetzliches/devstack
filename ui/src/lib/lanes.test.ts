import { describe, expect, it } from 'vitest';
import { notForwardedText, restartLanesText, restartPlan, runCounts, runsAnything } from './lanes';

describe('a started lane runs', () => {
  it('by its containers when the stack has no services', () => {
    const c = { running: 0, services: 0, containersRunning: 3, containers: 3 };
    expect(runsAnything(c)).toBe(true);
    expect(runCounts(c)).toBe('3 of 3 containers running');
  });

  it('nothing when it has services and containers and none of them runs', () => {
    const c = { running: 0, services: 2, containersRunning: 0, containers: 1 };
    expect(runsAnything(c)).toBe(false);
    expect(runCounts(c)).toBe('0 of 2 services, 0 of 1 container running');
  });

  it('as before without containers (an older daemon omits them)', () => {
    expect(runsAnything({ running: 1, services: 3, containersRunning: 0, containers: 0 })).toBe(true);
    expect(runCounts({ running: 1, services: 3, containersRunning: 0, containers: 0 })).toBe('1 of 3 services running');
    expect(runsAnything({ running: 0, services: 0, containersRunning: 0, containers: 0 })).toBe(true);
  });
});

describe('a restart of the stack (ADR-0052)', () => {
  const lane = (name: string, node?: string, active = true) => ({
    name, path: `/src/${name}`, number: 1, active, port_offset: 100, owns_main: false, running: 1, services: 1, ...(node ? { node } : {}),
  });

  it('says what happens to the lanes on nodes and here', () => {
    const p = restartPlan([lane('feat', 'build-server'), lane('fix'), lane('idle', undefined, false)], true);
    expect(p).toEqual({ onNodes: ['feat'], here: ['fix'], carried: true });
    expect(restartLanesText(p)).toBe('Lane feat keeps running on its node; lane fix restarts with it.');
    expect(restartLanesText(restartPlan([lane('a'), lane('b')], true))).toBe('Lanes a, b restart with it.');
  });

  it('warns that an older daemon stops its lanes', () => {
    expect(restartLanesText(restartPlan([lane('feat', 'build-server')], undefined))).toBe(
      'This daemon is too old to pass its lanes on: lane feat stops and must be started again.',
    );
  });

  it('says nothing without lanes', () => {
    expect(restartLanesText(restartPlan([], true))).toBe('');
  });
});

describe('ports a lane on a node does not forward', () => {
  it('are said by protocol and number', () => {
    expect(notForwardedText(undefined)).toBe('');
    expect(notForwardedText([])).toBe('');
    expect(notForwardedText(['5353/udp'])).toBe('UDP port 5353 is not forwarded to this machine (an older devstack forwards TCP only)');
    expect(notForwardedText(['5060/udp', '5353/udp'])).toBe('UDP ports 5060, 5353 are not forwarded to this machine (an older devstack forwards TCP only)');
    expect(notForwardedText(['9000/sctp'])).toBe('SCTP port 9000 is not forwarded to this machine (devstack forwards TCP and UDP only)');
    // The daemon's reason (ADR-0056): the node is too old to relay UDP.
    const why = 'the node build-1 runs devstack 0.19.0, which forwards TCP only: devstack 0.20.0 or later there relays UDP';
    expect(notForwardedText(['5353/udp', '9000/sctp'], why)).toBe(`ports UDP 5353, SCTP 9000 are not forwarded to this machine (${why})`);
  });
});
