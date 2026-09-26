import { deviceIdentity, sameDeviceIdentity, type DeviceInfo } from '@agent-device/kernel/device';
import { AppError } from '@agent-device/kernel/errors';
import { isRecord } from '@agent-device/kernel/record';
import { PendingTransferGuard } from '@agent-device/contracts/async-lifecycle';
import { sameRuntimeOwner, type RuntimeOwnerRef } from '@agent-device/contracts/platform-runtime';
import type { CleanupOutcome } from '@agent-device/contracts/durable-resource';
import type { ManagedProcessIdentity } from '@agent-device/contracts/platform-runtime-host';
import type {
  ScreenRecordingRuntimeHost,
  ScreenRecordingBackgroundProcess,
} from '@agent-device/contracts/screen-recording-runtime-host';
import {
  SCREEN_RECORDING_RESOURCE_KIND,
  type ScreenRecordingLiveSnapshot,
  type ScreenRecordingRuntimeOperations,
  type ScreenRecordingReattachInput,
  type ScreenRecordingStartInput,
} from '@agent-device/contracts/screen-recording-runtime';
import {
  createDurableResourceEnvelope,
  createScreenRecordingLiveHandle,
} from '@agent-device/capture-kit';
import { recordingFactsAreValid } from '@agent-device/capture-kit/recording-facts';
import {
  nativeRecordingPath,
  stopAndExportScreenRecording,
} from '@agent-device/capture-kit/recording-stop-sequence';

type Host = Pick<ScreenRecordingRuntimeHost, 'linux' | 'outputs' | 'finalize' | 'ownedProcesses'>;
type Coordinates = Omit<ScreenRecordingLiveSnapshot, 'gestureEvents'>;
type Descriptor = {
  nativePath: string;
  processes: readonly ManagedProcessIdentity[];
  recording: Coordinates;
};
const BACKEND = 'X11 ffmpeg';

export function createLinuxRecordingOperations(params: {
  host: Host;
  device: DeviceInfo;
  owner: RuntimeOwnerRef;
  signal: AbortSignal;
}): ScreenRecordingRuntimeOperations {
  const { host, device, owner, signal } = params;
  function decode(input: ScreenRecordingReattachInput): Descriptor | undefined {
    const { envelope } = input;
    if (
      envelope.descriptor.version !== 1 ||
      envelope.resourceKind !== SCREEN_RECORDING_RESOURCE_KIND ||
      !sameDeviceIdentity(envelope.device, deviceIdentity(device)) ||
      !sameRuntimeOwner(envelope.owner, owner)
    )
      return undefined;
    const body = envelope.descriptor.body;
    const recording = body.recording;
    if (
      body.backend !== 'x11-ffmpeg' ||
      !isRecord(recording) ||
      recording.backend !== BACKEND ||
      typeof recording.outPath !== 'string' ||
      !recording.outPath.startsWith('/') ||
      typeof recording.startedAt !== 'number' ||
      !Number.isFinite(recording.startedAt) ||
      (recording.clientOutPath !== undefined && typeof recording.clientOutPath !== 'string') ||
      (recording.scope === 'app' && !isRecord(recording.activeSessionApp)) ||
      typeof body.nativePath !== 'string' ||
      body.nativePath !== nativeRecordingPath(recording.outPath) ||
      !Array.isArray(body.processes) ||
      body.processes.length !== 2 ||
      !body.processes.every(validMarker) ||
      !recordingFactsAreValid(recording)
    )
      return undefined;
    return {
      nativePath: body.nativePath,
      processes: body.processes,
      recording: recording as Coordinates,
    };
  }
  return {
    screenRecordingStart: async (input) => {
      signal.throwIfAborted();
      validate(input);
      const nativePath = nativeRecordingPath(input.outputPath);
      await host.outputs.prepare(nativePath);
      const process = await host.linux.start(
        {
          outputPath: nativePath,
          fps: input.fps,
          ...(input.scope === 'app' ? { appId: input.activeSessionApp!.bundleId } : {}),
        },
        signal,
      );
      const markers = process.markers;
      try {
        signal.throwIfAborted();
        if (!markers || markers.length !== 2)
          throw new AppError(
            'COMMAND_FAILED',
            'Linux recorder did not expose its worker and encoder identities',
          );
        host.ownedProcesses.replace(
          { kind: 'session', sessionId: input.sessionId },
          markers.map((marker) => ({ ...marker, purpose: 'linux-screen-recording' })),
        );
        const recording: Coordinates = {
          backend: BACKEND,
          outPath: input.outputPath,
          startedAt: Date.now(),
          scope: input.scope,
          showTouches: input.showTouches,
          recordOnlySession: input.recordOnlySession,
          ...(input.clientOutputPath === undefined
            ? {}
            : { clientOutPath: input.clientOutputPath }),
          ...(input.activeSessionApp === undefined
            ? {}
            : { activeSessionApp: input.activeSessionApp }),
          ...(input.exportQuality === undefined ? {} : { exportQuality: input.exportQuality }),
        };
        const descriptor: Descriptor = { nativePath, processes: markers, recording };
        const handle = liveHandle(host, input.sessionId, descriptor, process);
        return {
          pendingHandle: new PendingTransferGuard(handle),
          envelope: createDurableResourceEnvelope({
            resourceKind: SCREEN_RECORDING_RESOURCE_KIND,
            sessionId: input.sessionId,
            device: deviceIdentity(device),
            owner,
            fence: input.fence,
            lifecycle: 'open',
            descriptor: {
              version: 1,
              body: {
                backend: 'x11-ffmpeg',
                nativePath,
                processes: markers.map((marker) => ({ ...marker })),
                recording: { ...recording },
              },
            },
          }),
        };
      } catch (error) {
        await process.terminate();
        await process.wait;
        host.ownedProcesses.clear({ kind: 'session', sessionId: input.sessionId });
        throw error;
      }
    },
    screenRecordingReattach: async (input) => {
      const descriptor = decode(input);
      if (!descriptor)
        return {
          status: 'unreattachable',
          reason: 'descriptor-invalid',
          message:
            'Linux recording descriptor does not match its owner, device or recording coordinates.',
        };
      const ownership = await Promise.all(
        descriptor.processes.map((marker) => host.linux.inspectProcess(marker)),
      );
      if (ownership.includes('ownership-lost'))
        return {
          status: 'unreattachable',
          reason: 'ownership-fence-lost',
          message: 'Linux recorder process identity changed.',
        };
      return { status: 'active', handle: liveHandle(host, input.envelope.sessionId, descriptor) };
    },
    screenRecordingCleanup: async (input) => {
      const descriptor = decode(input);
      if (!descriptor)
        return {
          status: 'cleanup-pending',
          reason: 'manual-recovery-required',
          message: 'Invalid Linux recording descriptor; no processes were signaled.',
        };
      return cleanup(host, input.envelope.sessionId, descriptor.processes);
    },
  };
}

function validate(input: ScreenRecordingStartInput): void {
  if (input.scope === 'app' && !input.activeSessionApp?.bundleId) {
    throw new AppError(
      'INVALID_ARGS',
      'App-scoped Linux recording requires an active named app session',
    );
  }
  if (!input.outputPath.startsWith('/') || !input.outputPath.toLowerCase().endsWith('.mp4'))
    throw new AppError('INVALID_ARGS', 'Linux recording requires an absolute .mp4 output path');
  if (input.fps !== undefined && (!Number.isInteger(input.fps) || input.fps < 1 || input.fps > 60))
    throw new AppError('INVALID_ARGS', 'Recording fps must be an integer between 1 and 60');
}

function validMarker(value: unknown): value is ManagedProcessIdentity {
  return (
    isRecord(value) &&
    typeof value.pid === 'number' &&
    Number.isSafeInteger(value.pid) &&
    value.pid > 0 &&
    typeof value.startTime === 'string' &&
    value.startTime.length > 0 &&
    typeof value.command === 'string' &&
    value.command.length > 0
  );
}

async function cleanup(
  host: Host,
  sessionId: string,
  processes: readonly ManagedProcessIdentity[],
  process?: ScreenRecordingBackgroundProcess,
): Promise<CleanupOutcome> {
  try {
    if (process) {
      await process.terminate();
      await process.wait;
    } else if ((await host.linux.terminateProcesses(processes)) === 'ownership-lost') {
      return {
        status: 'cleanup-pending',
        reason: 'ownership-fence-lost',
        message: 'Linux recorder process identity changed.',
      };
    }
    host.ownedProcesses.clear({ kind: 'session', sessionId });
    return { status: 'cleaned' };
  } catch (error) {
    return {
      status: 'cleanup-pending',
      reason: 'cleanup-unconfirmed',
      message: error instanceof Error ? error.message : String(error),
    };
  }
}

function liveHandle(
  host: Host,
  sessionId: string,
  descriptor: Descriptor,
  process?: ScreenRecordingBackgroundProcess,
) {
  const { nativePath, recording, processes } = descriptor;
  return createScreenRecordingLiveHandle(
    { ...recording, gestureEvents: [] },
    {
      forceCleanup: async () => cleanup(host, sessionId, processes, process),
      finish: async (snapshot, progress) =>
        stopAndExportScreenRecording({
          snapshot,
          progress,
          steps: {
            stop: async () => {
              const outcome = await cleanup(host, sessionId, processes, process);
              if (outcome.status === 'cleanup-pending')
                throw new AppError(
                  'COMMAND_FAILED',
                  outcome.message ?? 'Linux recording cleanup is unconfirmed',
                  { reason: outcome.reason },
                );
              if (process) {
                const result = await process.wait;
                if (result.exitCode !== 0)
                  throw new AppError(
                    'COMMAND_FAILED',
                    'Linux recording did not finalize successfully',
                    { stderr: result.stderr },
                  );
              }
              return {
                observation: { recorder: 'confirmed' },
                ...(process
                  ? {}
                  : {
                      warning:
                        'Recording resumed after daemon restart; the video covers only frames captured before the recorder stopped.',
                    }),
              };
            },
            collect: async (collectedPath) => {
              await host.outputs.copy({ from: nativePath, to: collectedPath });
              await host.finalize.sniff({ outputPath: collectedPath });
            },
            finalize: async ({ collectedPath, exportPath }) => {
              const overlayUnavailable =
                snapshot.invalidatedReason ??
                (!process && snapshot.showTouches
                  ? 'gesture events were lost when the daemon restarted'
                  : undefined);
              try {
                await host.outputs.copy({ from: collectedPath, to: exportPath });
                const result = await host.finalize.complete({
                  outputPath: exportPath,
                  showTouches: !overlayUnavailable && snapshot.showTouches,
                  gestureEvents: snapshot.gestureEvents,
                  exportQuality: snapshot.exportQuality ?? 'medium',
                  targetLabel: 'Linux recording',
                });
                return {
                  ...result,
                  ...(overlayUnavailable ? { overlayWarning: overlayUnavailable } : {}),
                  nativePathDisposition:
                    (await host.outputs.remove(nativePath)) === 'removed' ? 'retired' : 'retirable',
                };
              } catch (error) {
                await host.outputs.remove(exportPath);
                throw error;
              }
            },
            discard: async (collectedPath) => {
              await host.outputs.remove(collectedPath);
            },
          },
        }),
    },
  );
}
