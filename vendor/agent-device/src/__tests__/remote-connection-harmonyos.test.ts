import { expect, test } from 'vitest';
import fs from 'node:fs';
import {
  connectionWorkspace,
  createTestClient,
  forceConnectFlags,
  seedConnectionState,
} from './remote-connection.fixtures.ts';
import {
  materializeRemoteConnectionForCommand,
  resolveRequestedLeaseBackend,
} from '../cli/commands/connection-runtime.ts';
import { disconnectCommand } from '../cli/commands/connection.ts';
import { readRemoteConnectionState } from '../remote/remote-connection-state.ts';
import { LeaseRegistry } from '../daemon/lease-registry.ts';

test('HarmonyOS platform resolves to its proxy lease backend', () => {
  expect(
    resolveRequestedLeaseBackend(
      forceConnectFlags({
        stateDir: '/tmp/agent-device',
        remoteConfig: '/tmp/remote.json',
        platform: 'harmonyos',
      }),
    ),
  ).toBe('harmonyos-instance');
});

test.each(['apple', 'harmonyos', 'ios', 'android'] as const)(
  'stored Harmony runtime compatibility respects %s selection',
  async (platform) => {
    const { stateDir, remoteConfigPath } = connectionWorkspace('harmonyos-runtime-');
    fs.writeFileSync(remoteConfigPath, JSON.stringify({ daemonBaseUrl: 'https://daemon.example' }));
    seedConnectionState({
      stateDir,
      state: {
        session: 'runtime-compat',
        remoteConfigPath,
        daemon: { baseUrl: 'https://daemon.example' },
        tenant: 'proxy',
        runId: 'compat-run',
        leaseId: 'compat-existing',
        leaseBackend: 'harmonyos-instance',
        runtime: { platform: 'harmonyos', launchUrl: 'demo://open' },
      },
    });
    const materialized = await materializeRemoteConnectionForCommand({
      command: 'snapshot',
      client: createTestClient(),
      flags: {
        json: true,
        help: false,
        version: false,
        stateDir,
        remoteConfig: remoteConfigPath,
        session: 'runtime-compat',
        platform,
      },
    });
    if (platform === 'apple' || platform === 'harmonyos') {
      expect(materialized.runtime?.platform).toBe('harmonyos');
    } else {
      expect(materialized.runtime).toBeUndefined();
    }
  },
);

test('proxy HarmonyOS inventory materializes a scoped lease and closes that same lease', async () => {
  const { stateDir, remoteConfigPath } = connectionWorkspace('harmonyos-proxy-');
  fs.writeFileSync(remoteConfigPath, JSON.stringify({ daemonBaseUrl: 'https://daemon.example' }));
  seedConnectionState({
    stateDir,
    state: {
      session: 'harmony-proxy',
      remoteConfigPath,
      daemon: { baseUrl: 'https://daemon.example' },
      tenant: 'proxy',
      runId: 'harmony-run',
      leaseProvider: 'proxy',
      clientId: 'harmony-client',
    },
  });
  const registry = new LeaseRegistry();
  const allocated: string[] = [];
  const released: string[] = [];
  const client = createTestClient({
    listDevices: async () => [
      {
        platform: 'harmonyos',
        target: 'mobile',
        kind: 'emulator',
        id: '127.0.0.1:16001',
        name: 'Harmony Emulator',
        booted: true,
        identifiers: { serial: '127.0.0.1:16001' },
        harmonyos: { serial: '127.0.0.1:16001' },
      },
    ],
    allocate: async (request) => {
      expect(request.leaseBackend).toBe('harmonyos-instance');
      expect(request.deviceKey).toBe('harmonyos:mobile:127.0.0.1:16001');
      const lease = registry.allocateLease({ ...request, tenantId: request.tenant });
      allocated.push(lease.leaseId);
      return lease;
    },
    heartbeat: async (request) => registry.heartbeatLease({ ...request, tenantId: request.tenant }),
    release: async (request) => {
      const result = registry.releaseLease({ ...request, tenantId: request.tenant });
      expect(result.released).toBe(true);
      released.push(request.leaseId);
      return result;
    },
  });
  const flags = {
    json: true,
    help: false,
    version: false,
    stateDir,
    remoteConfig: remoteConfigPath,
    session: 'harmony-proxy',
  };
  const materialized = await materializeRemoteConnectionForCommand({
    command: 'open',
    flags,
    client,
  });
  expect(materialized.flags.platform).toBe('harmonyos');
  expect(materialized.flags.serial).toBe('127.0.0.1:16001');
  expect(materialized.flags.leaseBackend).toBe('harmonyos-instance');
  expect(materialized.flags.leaseId).toBe(allocated[0]);
  expect(readRemoteConnectionState({ stateDir, session: flags.session })?.leaseId).toBe(
    allocated[0],
  );
  expect(() =>
    registry.assertLeaseAdmission({
      leaseId: allocated[0],
      tenantId: 'proxy',
      runId: 'harmony-run',
      leaseBackend: 'android-instance',
    }),
  ).toThrow();
  await disconnectCommand({ positionals: [], flags, client });
  expect(released).toEqual(allocated);
  expect(readRemoteConnectionState({ stateDir, session: flags.session })).toBeNull();
});
