import { spawnSync } from 'node:child_process';
import type { ReplaySuiteResult } from '@agent-device/contracts/replay';

export type EngineResult = {
  engine: 'maestro' | 'agent-device';
  outcome: 'pass' | 'fail';
  exitCode: number;
  /** Failure provenance stays distinct from the behavioral comparison. */
  failureKind?: 'behavioral' | 'infrastructure';
};

export function runMaestroEngine(command: string, args: string[]): EngineResult {
  const [bin = '', ...rest] = command.split(' ').filter(Boolean);
  const result = spawnSync(bin, [...rest, ...args], { stdio: 'inherit', cwd: process.cwd() });
  // Maestro does not expose typed failure provenance. Until it does, a non-zero exit cannot
  // safely satisfy a behavioral divergence waiver.
  return buildEngineResult('maestro', result, () => 'infrastructure');
}

export function classifyAgentDeviceFailure(stdout: string): 'behavioral' | 'infrastructure' {
  try {
    const envelope = JSON.parse(stdout) as { data?: ReplaySuiteResult };
    const failures = envelope.data?.failures;
    if (!Array.isArray(failures) || failures.length === 0) return 'infrastructure';
    return failures.some((failure) => failure.infrastructure === true)
      ? 'infrastructure'
      : 'behavioral';
  } catch {
    // A non-zero process that did not return the promised suite envelope never reached a
    // classifiable behavioral oracle. Keep it red, but do not call it a divergence.
    return 'infrastructure';
  }
}

/**
 * The agent-device CLI the differential drives, as two variables that cannot be
 * confused for one another:
 *
 *   AGENT_DEVICE_CLI            the entry script — ONE path, never split, so a
 *                               path containing spaces survives verbatim
 *   AGENT_DEVICE_CLI_NODE_FLAGS node flags — split on whitespace, which is exact
 *                               because a node flag cannot contain a space
 *
 * One variable holding `--experimental-strip-types src/bin.ts` cannot express
 * both: splitting it corrupts `/tmp/agent device.mjs`, and not splitting it
 * spawns the whole line as a single node option — the bug that infrastructure-
 * failed every scenario from 2026-08-25. Set the flags variable to the empty
 * string to run an entry that needs none (a built `bin/agent-device.mjs`).
 */
const DEFAULT_CLI_ENTRY = 'src/bin.ts';
const DEFAULT_CLI_NODE_FLAGS = '--experimental-strip-types';

export function resolveAgentDeviceCliArgv(
  entry: string | undefined,
  nodeFlags: string | undefined,
): string[] {
  const flags = (nodeFlags ?? DEFAULT_CLI_NODE_FLAGS).split(/\s+/).filter(Boolean);
  return [...flags, entry?.trim() || DEFAULT_CLI_ENTRY];
}

/**
 * `cliArgv` is a node argv — flags and entry script as separate elements, built
 * by `resolveAgentDeviceCliArgv`. Never a command line: spawning one as a single
 * argument aborts node before the CLI loads ("bad option: --experimental-strip-
 * types src/bin.ts"), which every scenario then reports as an infrastructure
 * failure. An entry path reaching this array is already its own element.
 */
export function runAgentDeviceEngine(cliArgv: readonly string[], args: string[]): EngineResult {
  const result = spawnSync(process.execPath, [...cliArgv, ...args, '--json'], {
    cwd: process.cwd(),
    encoding: 'utf8',
  });
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  return buildEngineResult('agent-device', result, () => classifyAgentDeviceFailure(result.stdout));
}

function buildEngineResult(
  engine: EngineResult['engine'],
  result: { status: number | null; error?: Error },
  classifyFailure: () => NonNullable<EngineResult['failureKind']>,
): EngineResult {
  const exitCode = result.status ?? 1;
  if (exitCode === 0) return { engine, outcome: 'pass', exitCode };
  return {
    engine,
    outcome: 'fail',
    exitCode,
    failureKind:
      result.status === null || result.error !== undefined ? 'infrastructure' : classifyFailure(),
  };
}
