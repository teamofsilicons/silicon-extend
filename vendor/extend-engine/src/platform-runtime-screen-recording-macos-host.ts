import fs from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { AppError } from '@agent-device/kernel/errors';
import { startMacOsRecordingProcess } from '@agent-device/platform-apple/macos';
import type { ScreenRecordingBackgroundProcess } from '@agent-device/contracts/screen-recording-runtime-host';
import type { HostCommandResult } from '@agent-device/contracts/platform-runtime-host';
import {
  resolveManagedProcessIdentity,
  terminateManagedProcessSet,
} from './platform-runtime-screen-recording-process-host.ts';

const MACOS_RECORDING_FIRST_FRAME_TIMEOUT_MS = 15_000;

export async function startMacOsRecording(
  input: Readonly<{ outputPath: string; bundleId?: string; fps?: number }>,
  signal?: AbortSignal,
): Promise<ScreenRecordingBackgroundProcess> {
  signal?.throwIfAborted();
  const statusPath = `${input.outputPath}.${randomUUID()}.status.json`;
  const background = await startMacOsRecordingProcess({ ...input, statusPath });
  let result: HostCommandResult | undefined;
  let processError: unknown;
  void background.wait.then(
    (value) => {
      result = value;
    },
    (error: unknown) => {
      processError = error;
    },
  );
  const assertRunning = () => {
    if (processError) throw processError;
    if (result) throw recorderExitedDuringStartup(result);
  };
  let marker: Awaited<ReturnType<typeof resolveManagedProcessIdentity>>;
  try {
    const deadline = Date.now() + MACOS_RECORDING_FIRST_FRAME_TIMEOUT_MS;
    // Readiness is what the loop observed, not the clock after it: an identity lookup or status
    // read that finishes after the deadline must not turn a recorder that reported its first
    // frame into a startup failure that kills it.
    let ready = false;
    while (!ready && Date.now() < deadline) {
      signal?.throwIfAborted();
      assertRunning();
      marker ??= await resolveManagedProcessIdentity(background.child.pid);
      const status = await fs
        .readFile(statusPath, 'utf8')
        .then((text) => JSON.parse(text))
        .catch(() => undefined);
      assertRunning();
      ready = Boolean(
        marker && status?.state === 'recording' && status.pid === background.child.pid,
      );
      if (!ready) await delay(50, undefined, { signal });
    }
    if (!ready || !marker) {
      // A recorder that refused during the last wait says why; that beats a bare timeout.
      assertRunning();
      throw new AppError(
        'COMMAND_FAILED',
        `Native macOS recording did not produce its first frame within ${MACOS_RECORDING_FIRST_FRAME_TIMEOUT_MS / 1000} seconds, so it was stopped.`,
        {
          reason: 'recording_first_frame_timeout',
          hint: 'Make sure the app has a window on screen and that Silicon Extend has Screen Recording permission in System Settings > Privacy & Security, then run record start again.',
        },
      );
    }
    signal?.throwIfAborted();
  } catch (error) {
    background.child.kill('SIGTERM');
    const exited = await Promise.race([
      background.wait.then(
        () => true,
        () => true,
      ),
      delay(5000).then(() => false),
    ]);
    if (!exited) background.child.kill('SIGKILL');
    await background.wait.catch(() => undefined);
    await fs.rm(statusPath, { force: true });
    throw error;
  }
  const markers = [marker];
  let termination: Promise<void> | undefined;
  const terminate = () => {
    termination ??= terminateManagedProcessSet(markers, background)
      .then((outcome) => {
        if (outcome === 'ownership-lost')
          throw new Error('Native macOS recorder ownership changed before cleanup');
      })
      .catch((error: unknown) => {
        termination = undefined;
        throw error;
      });
    return termination;
  };
  const wait = background.wait.finally(async () => {
    await fs.rm(statusPath, { force: true });
  });
  return Object.freeze({ markers, wait, terminate });
}

/** The failure the native recorder printed as its last stdout line before it exited. */
type MacOsRecorderStartFailure = Readonly<{
  message: string;
  details: Readonly<Record<string, string>>;
}>;

function readMacOsRecorderStartFailure(stdout: string): MacOsRecorderStartFailure | undefined {
  const line = stdout.trim().split('\n').at(-1);
  if (!line) return undefined;
  let parsed: { ok?: unknown; error?: { message?: unknown; details?: unknown } };
  try {
    parsed = JSON.parse(line) as typeof parsed;
  } catch {
    return undefined;
  }
  if (parsed?.ok !== false || typeof parsed.error?.message !== 'string') return undefined;
  const raw = parsed.error.details;
  const details: Record<string, string> = {};
  if (raw !== null && typeof raw === 'object') {
    for (const [key, value] of Object.entries(raw)) {
      if (typeof value === 'string') details[key] = value;
    }
  }
  return { message: parsed.error.message.trim(), details };
}

const MACOS_RECORDING_PERMISSION_HINT =
  'Allow Screen Recording for Silicon Extend in System Settings > Privacy & Security > Screen & System Audio Recording, then run record start again.';

const MACOS_RECORDING_START_HINTS: Readonly<Record<string, string>> = {
  app_window_not_on_screen:
    "Show one of the app's windows on the current Space (unhide the app or unminimize the window), then run record start again. To record the whole screen instead, run record start --scope device.",
  app_not_available:
    'Open the app and show one of its windows, then run record start again. To record the whole screen instead, run record start --scope device.',
  screen_recording_permission_denied: MACOS_RECORDING_PERMISSION_HINT,
};

const MACOS_RECORDING_START_FALLBACK_HINT =
  'Check that Silicon Extend has Screen Recording permission in System Settings > Privacy & Security and that the app has a window on screen, then run record start again. The recorder output is in the error details.';

/**
 * The recorder ended before it reported its first frame. What the Silicon needs is the reason
 * the recorder gave (an app with no window on screen, a missing permission), which the recorder
 * prints as a JSON failure line; the exit alone says neither why nor what to do.
 */
function recorderExitedDuringStartup(result: HostCommandResult): AppError {
  const diagnostics = {
    exitCode: result.exitCode,
    ...(result.signal === undefined ? {} : { signal: result.signal }),
    stdout: result.stdout,
    stderr: result.stderr,
  };
  const failure = readMacOsRecorderStartFailure(result.stdout);
  if (!failure) {
    const exit =
      result.exitCode === null
        ? `was stopped${result.signal ? ` by ${result.signal}` : ''}`
        : `exited with code ${result.exitCode}`;
    return new AppError(
      'COMMAND_FAILED',
      `The native macOS recorder ${exit} before it recorded its first frame, and it gave no reason, so no recording was started.`,
      {
        ...diagnostics,
        reason: 'recording_start_failed',
        hint: MACOS_RECORDING_START_FALLBACK_HINT,
      },
    );
  }
  const reason =
    failure.details.reason ??
    (failure.details.permission === 'screen-recording'
      ? 'screen_recording_permission_denied'
      : 'recording_start_failed');
  return new AppError(
    'COMMAND_FAILED',
    `Native macOS recording could not start: ${failure.message}`,
    {
      ...failure.details,
      ...diagnostics,
      reason,
      hint: MACOS_RECORDING_START_HINTS[reason] ?? MACOS_RECORDING_START_FALLBACK_HINT,
    },
  );
}
