import type { DoctorCheck } from '@agent-device/contracts/observability';
import { inspectManagedAgentBrowserProcesses } from './agent-browser-lifecycle.ts';
import { getManagedAgentBrowserStatus } from './agent-browser-tool.ts';

export async function webBrowserLifecycleCheck(stateDir: string): Promise<DoctorCheck> {
  const status = getManagedAgentBrowserStatus({ stateDir });
  if (!status.installed) {
    return {
      id: 'web-agent-browser-processes',
      status: 'info',
      summary:
        'Managed web backend is not installed; no browser processes owned by the device engine counted.',
      evidence: { stateDir, installed: false },
    };
  }
  try {
    const summary = await inspectManagedAgentBrowserProcesses(status);
    return {
      id: 'web-agent-browser-processes',
      status: summary.count > 0 ? 'info' : 'pass',
      summary:
        summary.count > 0
          ? `${summary.count} live Chrome process${summary.count === 1 ? '' : 'es'} owned by the device engine detected.`
          : 'No live Chrome processes owned by the device engine detected.',
      evidence: {
        stateDir,
        installed: true,
        count: summary.count,
        pids: summary.pids,
        matchReasons: summary.processes.map((match) => match.reason),
      },
    };
  } catch (error) {
    return {
      id: 'web-agent-browser-processes',
      status: 'info',
      summary: 'Could not inspect live Chrome processes owned by the device engine.',
      hint: 'Run doctor again from a shell with permission to inspect local processes.',
      evidence: { stateDir, error: error instanceof Error ? error.message : String(error) },
    };
  }
}
