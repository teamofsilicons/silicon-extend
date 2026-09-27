import type { RecordOptions } from '@agent-device/contracts/client';
import {
  RETIRED_SCREENSHOT_MAX_SIZE,
  validateNoRetiredScreenshotMaxSize,
} from '@agent-device/contracts/capture';
import {
  RECORDING_EXPORT_QUALITIES,
  RECORDING_SCOPE_VALUES,
} from '@agent-device/contracts/recording';
import { AppError } from '@agent-device/kernel/errors';
import type { CommandSchemaOverride } from '@agent-device/command-registry/command-schema';
import type { FlagKey } from '@agent-device/command-registry/flag-types';
import { commonInputFromFlags, direct, optionalString } from '../cli-grammar/common.ts';
import type { CliReader, DaemonWriter } from '../cli-grammar/types.ts';
import {
  booleanField,
  enumField,
  integerField,
  requiredField,
  retiredField,
  stringField,
} from '../command-input.ts';
import { defineCommandFacet, defineCommandFamilyFromFacets } from '../family/types.ts';
import { defineFieldCommandMetadata } from '../field-command-contract.ts';
import { recordingCliOutputFormatters } from './output.ts';

const RECORD_COMMAND_NAME = 'record';
const TRACE_COMMAND_NAME = 'trace';
const RECORDING_ACTION_VALUES = ['start', 'stop'] as const;

const recordCommandDescription =
  'Start or stop a screen recording for the active app session or, where supported, the selected device, or build a contact-sheet PNG from a recording already exported. Long Android recordings can return multiple video artifacts; HarmonyOS supports whole-screen recording on physical devices; Linux X11 supports whole-screen and single-window app recording.';
const traceCommandDescription =
  'Start or stop trace-log capture and return the resulting artifact when capture ends. Use the same artifact path for the matching start and stop requests when an explicit path is required.';

// `record stop` prints the artifact path, which callers capture into a variable.
const recordCommandOptions = { parseableOutput: true } as const;
export const recordCommandMetadata = defineFieldCommandMetadata(
  RECORD_COMMAND_NAME,
  recordCommandDescription,
  {
    action: requiredField(enumField(RECORDING_ACTION_VALUES)),
    path: stringField(),
    fps: integerField(),
    maxSize: retiredField(RETIRED_SCREENSHOT_MAX_SIZE.migration.record),
    quality: enumField(RECORDING_EXPORT_QUALITIES),
    hideTouches: booleanField(),
    recordingScope: enumField(RECORDING_SCOPE_VALUES),
  },
  recordCommandOptions,
);

export const traceCommandMetadata = defineFieldCommandMetadata(
  TRACE_COMMAND_NAME,
  traceCommandDescription,
  {
    action: requiredField(enumField(RECORDING_ACTION_VALUES)),
    path: stringField(),
  },
);

/**
 * Which `record` action reads which option.
 *
 * A new option has to name the action that reads it. That is what keeps one shared list from letting
 * `record start --out take.mp4` look like it recorded to that path, and `record contact-sheet
 * clip.mp4 --fps 30` look like frame sampling was configured. The parser is handed this table as
 * `flagsByAction` and refuses an option the action cannot read.
 */
const RECORD_FLAGS_BY_ACTION: Readonly<Record<string, readonly FlagKey[]>> = {
  start: ['recordingScope', 'fps', 'quality', 'hideTouches'],
  // `record stop` takes a session and a target, and reads no recording option.
  stop: [],
  'contact-sheet': ['out'],
};

const RECORD_CLI_FLAGS: readonly FlagKey[] = [
  ...new Set(Object.values(RECORD_FLAGS_BY_ACTION).flat()),
];

const recordCliSchema = {
  usageOverride:
    'record start [path] [--scope <app|device|system>] [--fps <n>] [--quality <medium|high>] [--hide-touches] | record stop | record contact-sheet <video.mp4> [--out <sheet.png>]',
  usageFlags: [],
  listUsageOverride: 'record start [path] | record stop | record contact-sheet <video>',
  positionalArgs: ['start|stop|contact-sheet', 'path?'],
  allowedFlags: RECORD_CLI_FLAGS,
  flagsByAction: RECORD_FLAGS_BY_ACTION,
} as const satisfies CommandSchemaOverride;

const traceCliSchema = {
  usageOverride: 'trace start <path> | trace stop <path>',
  listUsageOverride: 'trace start <path> | trace stop <path>',
  positionalArgs: ['start|stop', 'path?'],
} as const satisfies CommandSchemaOverride;

export const recordCliReader: CliReader = (positionals, flags) => ({
  ...commonInputFromFlags(flags),
  action: readRecordingAction(positionals[0], RECORD_COMMAND_NAME),
  path: positionals[1],
  fps: flags.fps,
  quality: flags.quality as RecordOptions['quality'],
  hideTouches: flags.hideTouches,
  recordingScope: flags.recordingScope,
});

export const traceCliReader: CliReader = (positionals, flags) => ({
  ...commonInputFromFlags(flags),
  action: readRecordingAction(positionals[0], TRACE_COMMAND_NAME),
  path: positionals[1],
});

const recordDirectWriter = direct(RECORD_COMMAND_NAME, (input) =>
  recordingPositionals(input as RecordOptions),
);

export const recordDaemonWriter: DaemonWriter = (input) => {
  validateNoRetiredScreenshotMaxSize('record', input);
  return recordDirectWriter(input);
};

export const traceDaemonWriter: DaemonWriter = direct(TRACE_COMMAND_NAME, (input) =>
  recordingPositionals(input as RecordOptions),
);

export const recordCommandFacet = defineCommandFacet({
  name: RECORD_COMMAND_NAME,
  text: {
    summary: 'Start or stop screen recording',
    cliDetail:
      'The default --scope app requires an active app session from open <app>; use --scope device/system to explicitly request whole-screen recording where the selected backend supports it. Android record start publishes a durable device manifest, recordings longer than the 180s adb screenrecord limit are returned as multiple MP4 chunks while the daemon stays alive, and daemon-restart recovery uses only manifest-owned chunks. Android screenrecord encodes a frame only when the screen changes, so a clip can end at the last frame the recorder encoded instead of at record stop; durationMs is host wall clock from record start until the export finished, and capturedDurationMs reports the video timeline when it can be measured, with a warning naming how much of the window that video covers. An Android manifest left by an unreachable recording is retired on the next start once its recorders are proven gone; one still owned refuses with non-retriable DEVICE_IN_USE and reason native_recovery_evidence_open naming the session to run record stop for, and a recorder that is still writing an artifact refuses with DEVICE_IN_USE and reason native_recording_artifact_claimed, which clears itself once that recorder ends at the 180s limit. Linux X11 supports --scope device/system with ffmpeg, ffprobe, python3 and xwininfo, including --fps, --hide-touches and --quality: Linux exports the H.264 video its recorder encoded, unchanged, so --quality picks the bit rate of that encode (medium 8 Mbit/s, the default, or high 20 Mbit/s). Linux video has a constant frame rate on wall-clock time: when capture cannot keep up with --fps, frames repeat instead of the video playing back faster. Linux --scope app also needs xdotool, libXcomposite, libXdamage and libXfixes, and an active named app whose executable or desktop-file basename matches exactly one mapped WM_CLASS. It captures that window even when another app covers it. At record start the whole window is drawn again (the recorder clears it with exposures, so the X server paints any background and the app redraws the rest; the window may flicker once), and recording begins once every pixel of it has been, and an app that answers X11 pings (_NET_WM_PING, as GTK, Qt and Chromium-based apps do) must also answer one first. An app that misses either within 5 seconds of record start, such as one that is not responding, is refused, because its frames could otherwise show whatever covers, or last covered, its window; record the whole screen with --scope device instead. The part of a shaped window outside its outline records black. Starting an app recording briefly holds the X server while the recorder redirects the window. Resizing, unmapping or remapping the window ends the recording. Multiple matching windows are refused. Wayland portal recording is not implemented yet. The X11 recorder stops at 30 minutes or 1 GiB. It also stops when the daemon that started it exits; record stop after the daemon restarts still exports the video up to that point. HarmonyOS supports whole-screen recording on physical devices only: use --scope device/system; --fps, --quality, and --hide-touches are unsupported. Use --quality to choose medium or high export quality on supported backends. An iOS simulator host recording lock returns non-retriable DEVICE_IN_USE with reason apple_simulator_recording_busy. Stop the recording in its owning session; if a dead recorder left the host locked, ask the host operator to restart the CoreSimulator stream service. A record stop that cannot produce its export keeps what a retry reads back — on Android the device-side artifact and the native manifest — leaves the recording manifest of that session open, and returns its own error, so retry record stop in that session; only closing the session disposes them. Every stopped recording also reports what became of its recorder and of the file that recorder writes to: recorder is confirmed, or lost when the session holding it died, and nativePathDisposition is retirable while that file still owes a removal or retired once its removal was verified. Both are optional disclosures, not failures; ADR 0024 declares further states (unconfirmed, pending) for the steps that gain those probes, and no stop reports them yet. record contact-sheet <video.mp4> [--out <sheet.png>] is a local report over a file you already exported: it decodes frames out of that MP4 and writes one PNG holding the cells where the screen visibly changed, each labeled with its elapsed time on the clip timeline. It needs no session and no device, rebuilds any past recording, and works for every backend that exports MP4; a WebM recording is refused with reason contact_sheet_container_unsupported. The sample grid is bounded and spread across the whole clip, so it reports coverage rather than a review: a flash that opens and closes entirely between two sample times is not in the sheet, which is why the result also reports how many times it sampled and how many frames came back. Frame decoding is Apple AVFoundation tooling, so the command runs only on macOS hosts and refuses elsewhere with reason contact_sheet_unsupported_host.',
  },
  metadata: recordCommandMetadata,
  run: (client, input) => client.recording.record(input as RecordOptions),
  cliSchema: recordCliSchema,
  cliReader: recordCliReader,
  daemonWriter: recordDaemonWriter,
  cliOutputFormatter: recordingCliOutputFormatters.record,
});

export const traceCommandFacet = defineCommandFacet({
  name: TRACE_COMMAND_NAME,
  text: {
    summary: 'Start or stop trace capture',
    cliDetail: 'Pass that path as the same positional argument to start and stop.',
  },
  metadata: traceCommandMetadata,
  run: (client, input) => client.recording.trace(input),
  cliSchema: traceCliSchema,
  cliReader: traceCliReader,
  daemonWriter: traceDaemonWriter,
});

export const recordingCommandFamily = defineCommandFamilyFromFacets({
  name: 'recording',
  commands: [recordCommandFacet, traceCommandFacet],
});

function recordingPositionals(input: RecordOptions): string[] {
  return [input.action, ...optionalString(input.path)];
}

function readRecordingAction(value: string | undefined, command: string): 'start' | 'stop' {
  if (value === 'start' || value === 'stop') return value;
  throw new AppError('INVALID_ARGS', `${command} requires start|stop`);
}
