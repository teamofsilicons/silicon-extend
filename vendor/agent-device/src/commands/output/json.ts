import type { NormalizedError } from '@agent-device/kernel/errors';

type JsonResult =
  | { success: true; data?: unknown; text?: string }
  | { success: false; error: NormalizedError };

export function printJson(result: JsonResult): void {
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

/**
 * Silicon Extend fork: with `AGENT_DEVICE_JSON_TEXT=1`, `--json` output also carries the text the
 * command prints without `--json`, so a relay can return both from one run.
 */
export function jsonTextRequested(): boolean {
  return process.env.AGENT_DEVICE_JSON_TEXT === '1';
}
