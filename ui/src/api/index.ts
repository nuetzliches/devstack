import type { Transport } from './transport';
import { RealTransport } from './real';

export const isMock = import.meta.env.VITE_MOCK === '1';

let transport: Transport | null = null;

/** Picks the transport once at startup. The mock is only bundled in mock builds. */
export async function initTransport(): Promise<Transport> {
  if (transport) return transport;
  if (isMock) {
    const { MockTransport } = await import('./mock');
    transport = new MockTransport();
  } else {
    transport = new RealTransport();
  }
  return transport;
}

export function api(): Transport {
  if (!transport) throw new Error('transport not initialised');
  return transport;
}

export * from './transport';
export type * from './types';
