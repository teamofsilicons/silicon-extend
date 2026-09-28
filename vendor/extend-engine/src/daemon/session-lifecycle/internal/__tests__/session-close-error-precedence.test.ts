import { beforeEach, expect, test } from 'vitest';
import {
  sessionCloseShutdownFixture,
  type SessionState,
} from './session-close-shutdown.fixtures.ts';

const {
  acquireDeviceClaim,
  AppError,
  flushDiagnosticsToSessionFile,
  fs,
  handleSessionCommands,
  inspectDeviceClaims,
  LeaseRegistry,
  makeIosSimulatorRecordingSession,
  makeSession,
  makeSessionStore,
  mkdtempForTestSync,
  mockDispatchCommand,
  mockReleaseRunnerOnClose,
  mockStopIosRunnerSession,
  noopInvoke,
  path,
  resetSessionCloseShutdownMocks,
  withDiagnosticsScope,
} = sessionCloseShutdownFixture;

beforeEach(resetSessionCloseShutdownMocks);

test('targeted close preserves the platform-close AppError and still runs later cleanup', async () => {
  const sessionStore = makeSessionStore();
  const sessionName = 'targeted-close-error-session';
  const session = makeIosSimulatorRecordingSession(sessionStore, sessionName, {
    device: {
      platform: 'apple',
      id: 'sim-udid-close-error',
      name: 'iPhone 15',
      kind: 'simulator',
      booted: true,
    },
  });
  sessionStore.set(sessionName, session);

  const platformCloseError = new AppError('DEVICE_UNAVAILABLE', 'platform close failed', {
    reason: 'device_disconnected',
    hint: 'Reconnect the device and retry close.',
  });
  mockDispatchCommand.mockRejectedValueOnce(platformCloseError);

  await expect(
    handleSessionCommands({
      req: {
        token: 't',
        session: sessionName,
        command: 'close',
        positionals: ['com.example.app'],
        flags: {},
      },
      sessionName,
      logPath: path.join(mkdtempForTestSync('daemon'), 'daemon.log'),
      sessionStore,
      invoke: noopInvoke,
    }),
  ).rejects.toBe(platformCloseError);

  // The original AppError code/details/hint are preserved, not collapsed into a
  // generic cleanup aggregate.
  expect(platformCloseError.code).toBe('DEVICE_UNAVAILABLE');
  expect(platformCloseError.details).toMatchObject({
    reason: 'device_disconnected',
    hint: 'Reconnect the device and retry close.',
  });
  // A failed close is not recorded as `Closed`.
  expect(session.actions.some((action) => action.command === 'close')).toBe(false);
  // Independent cleanup still runs; the session remains available for retry.
  expect(mockReleaseRunnerOnClose).toHaveBeenCalledWith(session.device.id, { retain: false });
  expect(sessionStore.get(sessionName)).toBeDefined();
});

test('a failed platform close retains the session and claim until a successful retry', async () => {
  const claimsRoot = mkdtempForTestSync('agent-device-session-close-claim-retained-');
  const previousClaimsDir = process.env.AGENT_DEVICE_CLAIMS_DIR;
  process.env.AGENT_DEVICE_CLAIMS_DIR = claimsRoot;
  try {
    const sessionStore = makeSessionStore();
    const sessionName = 'targeted-close-claim-retained-session';
    const device = {
      platform: 'apple' as const,
      id: 'sim-udid-close-claim-retained',
      name: 'iPhone 15',
      kind: 'simulator' as const,
      booted: true,
    };
    const acquired = await acquireDeviceClaim({
      device,
      session: sessionName,
      workspace: process.cwd(),
      stateDir: sessionStore.resolveDaemonStateDir(),
      reconcileOrphanedDeviceClaim: async () => ({
        status: 'retained',
        reason: 'test-no-recovery',
      }),
    });
    if (acquired.status !== 'acquired') {
      throw new Error('expected the test session to acquire a device claim');
    }
    const session = makeIosSimulatorRecordingSession(sessionStore, sessionName, { device });
    session.deviceClaim = acquired.ownership;
    sessionStore.set(sessionName, session);

    const platformCloseError = new AppError('DEVICE_UNAVAILABLE', 'platform close failed', {
      reason: 'device_disconnected',
      hint: 'Reconnect the device and retry close.',
    });
    mockDispatchCommand.mockRejectedValueOnce(platformCloseError);

    const diagnosticsLogPath = path.join(claimsRoot, 'diagnostics.ndjson');
    const thrown = await withDiagnosticsScope(
      { session: sessionName, command: 'close', logPath: diagnosticsLogPath },
      async () => {
        let caught: unknown;
        try {
          await handleSessionCommands({
            req: {
              token: 't',
              session: sessionName,
              command: 'close',
              positionals: ['com.example.app'],
              flags: {},
            },
            sessionName,
            logPath: path.join(mkdtempForTestSync('daemon'), 'daemon.log'),
            sessionStore,
            invoke: noopInvoke,
          });
        } catch (error) {
          caught = error;
        }
        flushDiagnosticsToSessionFile({ force: true });
        return caught;
      },
    );

    expect(thrown).toBe(platformCloseError);
    expect(sessionStore.get(sessionName)).toBeDefined();

    // Retain ownership until cleanup is confirmed.
    const claimState = inspectDeviceClaims({ serial: device.id })[0];
    expect(claimState?.classification).toBe('live');
    expect(claimState?.claim?.session).toBe(sessionName);

    // A warn diagnostic names the retained claim's device key and owning session so the retention
    // is observable instead of silent.
    const rows = fs
      .readFileSync(diagnosticsLogPath, 'utf8')
      .trim()
      .split('\n')
      .map((line) => JSON.parse(line));
    expect(rows).toContainEqual(
      expect.objectContaining({
        level: 'warn',
        phase: 'device_claim_close_effects_unconfirmed',
        data: { deviceKey: acquired.ownership.deviceKey, session: sessionName },
      }),
    );
    const retried = await handleSessionCommands({
      req: { token: 't', session: sessionName, command: 'close', positionals: [], flags: {} },
      sessionName,
      logPath: path.join(claimsRoot, 'retry.log'),
      sessionStore,
      invoke: noopInvoke,
    });
    expect(retried?.ok).toBe(true);
    expect(sessionStore.get(sessionName)).toBeUndefined();
    expect(inspectDeviceClaims({ serial: device.id })).toEqual([]);
  } finally {
    if (previousClaimsDir === undefined) delete process.env.AGENT_DEVICE_CLAIMS_DIR;
    else process.env.AGENT_DEVICE_CLAIMS_DIR = previousClaimsDir;
    fs.rmSync(claimsRoot, { recursive: true, force: true });
  }
});

// The retention decision has TWO inputs — `platformCloseError ?? cleanupAggregate` — and the test
// above only drives the first. A failed recording finish reaches retention through a
// different value, so it needs its own pin: making the aggregate stop blocking the claim would
// leave the test above green.
test('failed resource cleanup retains the session and claim until a successful retry', async () => {
  const claimsRoot = mkdtempForTestSync('agent-device-session-close-claim-cleanup-failure-');
  const previousClaimsDir = process.env.AGENT_DEVICE_CLAIMS_DIR;
  process.env.AGENT_DEVICE_CLAIMS_DIR = claimsRoot;
  try {
    const sessionStore = makeSessionStore();
    const sessionName = 'close-claim-cleanup-failure-session';
    const device = {
      platform: 'apple' as const,
      id: 'sim-recording-failed-close',
      name: 'iPhone',
      kind: 'simulator' as const,
      booted: true,
    };
    const acquired = await acquireDeviceClaim({
      device,
      session: sessionName,
      workspace: process.cwd(),
      stateDir: sessionStore.resolveDaemonStateDir(),
      reconcileOrphanedDeviceClaim: async () => ({
        status: 'retained',
        reason: 'test-no-recovery',
      }),
    });
    if (acquired.status !== 'acquired') {
      throw new Error('expected the test session to acquire a device claim');
    }
    const session = makeIosSimulatorRecordingSession(sessionStore, sessionName, {
      device,
      recorderExitCode: 1,
    });
    session.deviceClaim = acquired.ownership;
    const leaseRegistry = new LeaseRegistry();
    const lease = leaseRegistry.allocateLease({
      tenantId: 'tenant-a',
      runId: 'recording-close',
      leaseProvider: 'test-provider',
      deviceKey: 'apple:' + device.id,
      clientId: 'client-a',
    });
    session.lease = {
      leaseId: lease.leaseId,
      tenantId: lease.tenantId,
      runId: lease.runId,
      leaseBackend: lease.backend,
      leaseProvider: lease.leaseProvider,
      deviceKey: lease.deviceKey,
      clientId: lease.clientId,
      expiresAt: lease.expiresAt,
    };
    let releaseAttempts = 0;
    const leaseLifecycleProvider = {
      release: async () => {
        releaseAttempts += 1;
        return {};
      },
    };
    sessionStore.set(sessionName, session);

    // The platform close itself succeeds; only the best-effort cleanup step fails, so the
    // blocking error arrives as the cleanup aggregate rather than as platformCloseError.
    const diagnosticsLogPath = path.join(claimsRoot, 'diagnostics.ndjson');
    const thrown = await withDiagnosticsScope(
      { session: sessionName, command: 'close', logPath: diagnosticsLogPath },
      async () => {
        let caught: unknown;
        try {
          await handleSessionCommands({
            req: {
              token: 't',
              session: sessionName,
              command: 'close',
              positionals: [],
              flags: {},
            },
            sessionName,
            logPath: path.join(mkdtempForTestSync('daemon'), 'daemon.log'),
            sessionStore,
            leaseRegistry,
            leaseLifecycleProvider,
            invoke: noopInvoke,
          });
        } catch (error) {
          caught = error;
        }
        flushDiagnosticsToSessionFile({ force: true });
        return caught;
      },
    );

    expect(thrown).toMatchObject({
      details: expect.objectContaining({
        reason: 'session_cleanup_incomplete',
        failedSteps: ['recording'],
      }),
    });
    expect(sessionStore.get(sessionName)).toBeDefined();
    expect(releaseAttempts).toBe(0);
    expect(leaseRegistry.listActiveLeases()).toHaveLength(1);

    const claimState = inspectDeviceClaims({ serial: device.id })[0];
    expect(claimState?.classification).toBe('live');
    expect(claimState?.claim?.session).toBe(sessionName);

    const rows = fs
      .readFileSync(diagnosticsLogPath, 'utf8')
      .trim()
      .split('\n')
      .map((line) => JSON.parse(line));
    expect(rows).toContainEqual(
      expect.objectContaining({
        level: 'warn',
        phase: 'device_claim_close_effects_unconfirmed',
        data: { deviceKey: acquired.ownership.deviceKey, session: sessionName },
      }),
    );
    const retried = await handleSessionCommands({
      req: { token: 't', session: sessionName, command: 'close', positionals: [], flags: {} },
      sessionName,
      logPath: path.join(claimsRoot, 'retry.log'),
      sessionStore,
      leaseRegistry,
      leaseLifecycleProvider,
      invoke: noopInvoke,
    });
    expect(retried?.ok).toBe(true);
    expect(releaseAttempts).toBe(1);
    expect(leaseRegistry.listActiveLeases()).toHaveLength(0);
    expect(sessionStore.get(sessionName)).toBeUndefined();
    expect(inspectDeviceClaims({ serial: device.id })).toEqual([]);
  } finally {
    if (previousClaimsDir === undefined) delete process.env.AGENT_DEVICE_CLAIMS_DIR;
    else process.env.AGENT_DEVICE_CLAIMS_DIR = previousClaimsDir;
    fs.rmSync(claimsRoot, { recursive: true, force: true });
  }
});

test('a successful close clears the device claim', async () => {
  const claimsRoot = mkdtempForTestSync('agent-device-session-close-claim-cleared-');
  const previousClaimsDir = process.env.AGENT_DEVICE_CLAIMS_DIR;
  process.env.AGENT_DEVICE_CLAIMS_DIR = claimsRoot;
  try {
    const sessionStore = makeSessionStore();
    const sessionName = 'targeted-close-claim-cleared-session';
    const device = {
      platform: 'android' as const,
      id: 'emulator-5554',
      name: 'Pixel',
      kind: 'emulator' as const,
      booted: true,
    };
    const acquired = await acquireDeviceClaim({
      device,
      session: sessionName,
      workspace: process.cwd(),
      stateDir: sessionStore.resolveDaemonStateDir(),
      reconcileOrphanedDeviceClaim: async () => ({
        status: 'retained',
        reason: 'test-no-recovery',
      }),
    });
    if (acquired.status !== 'acquired') {
      throw new Error('expected the test session to acquire a device claim');
    }
    const session = {
      ...makeSession(sessionName, device),
      deviceClaim: acquired.ownership,
    };
    sessionStore.set(sessionName, session);

    const response = await handleSessionCommands({
      req: {
        token: 't',
        session: sessionName,
        command: 'close',
        positionals: [],
        flags: {},
      },
      sessionName,
      logPath: path.join(mkdtempForTestSync('daemon'), 'daemon.log'),
      sessionStore,
      invoke: noopInvoke,
    });

    expect(response?.ok).toBe(true);
    expect(sessionStore.get(sessionName)).toBeUndefined();
    expect(inspectDeviceClaims({ serial: device.id })).toEqual([]);
  } finally {
    if (previousClaimsDir === undefined) delete process.env.AGENT_DEVICE_CLAIMS_DIR;
    else process.env.AGENT_DEVICE_CLAIMS_DIR = previousClaimsDir;
    fs.rmSync(claimsRoot, { recursive: true, force: true });
  }
});

test('targeted close skips platform dispatch and preserves the error when the required pre-close runner stop fails', async () => {
  const sessionStore = makeSessionStore();
  const sessionName = 'targeted-close-preclose-failure-session';
  // A physical Apple target (non-simulator) must stop its runner before the
  // platform close is dispatched — the runner owns the device connection.
  const session = {
    ...makeSession(sessionName, {
      platform: 'apple',
      id: 'physical-device-close',
      name: 'My iPhone',
      kind: 'device',
      booted: true,
    }),
    appBundleId: 'com.example.app',
  } as unknown as SessionState;
  sessionStore.set(sessionName, session);

  const preCloseError = new AppError('RUNNER_UNAVAILABLE', 'runner stop failed', {
    reason: 'runner_stop_failed',
    hint: 'Retry once the runner is reachable.',
  });
  // Scoped to this test's own two internal calls (pre-close stop, then the later
  // independent-cleanup retry) — a persistent `mockRejectedValue` here would leak into every
  // later test that exercises an Apple-platform runner stop, since `vi.clearAllMocks()` in
  // `beforeEach` clears call history but not a mock's implementation.
  mockStopIosRunnerSession.mockRejectedValueOnce(preCloseError);

  await expect(
    handleSessionCommands({
      req: {
        token: 't',
        session: sessionName,
        command: 'close',
        positionals: ['com.example.app'],
        flags: {},
      },
      sessionName,
      logPath: path.join(mkdtempForTestSync('daemon'), 'daemon.log'),
      sessionStore,
      invoke: noopInvoke,
    }),
  ).rejects.toBe(preCloseError);

  // The platform close must not be dispatched after a required pre-close stop fails.
  expect(mockDispatchCommand).not.toHaveBeenCalled();
  // The original failure code/details/hint are preserved, not collapsed into a
  // generic cleanup aggregate.
  expect(preCloseError.code).toBe('RUNNER_UNAVAILABLE');
  expect(preCloseError.details).toMatchObject({
    reason: 'runner_stop_failed',
    hint: 'Retry once the runner is reachable.',
  });
  // A skipped close is not recorded as `Closed`.
  expect(session.actions.some((action) => action.command === 'close')).toBe(false);
  // Independent cleanup still runs; the session remains available for retry.
  expect(mockStopIosRunnerSession).toHaveBeenCalledOnce();
  expect(mockReleaseRunnerOnClose).toHaveBeenCalledWith(session.device.id, { retain: false });
  expect(sessionStore.get(sessionName)).toBeDefined();
});

// Live evidence (2026-08-02): a plain `open` followed by `close --save-script` used to fold the
// never-armed session into the authoring lifecycle and publish anyway, producing a script with
// selector fallback chains but no recording-time `target-v1` evidence. These two tests prove the
// daemon-seam fix: the rejection fires before ANY teardown work (no dispatch mock needed — a
// no-target close on Android never reaches `dispatchCommand`), the session survives so the agent
// can retry, and no script file is written.
