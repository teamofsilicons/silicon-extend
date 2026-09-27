// Device-free self-test for the layer-3 differential registry. Runs in unit CI
// via node --test; the live device comparison itself runs only on the scheduled
// conformance-differential workflow.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'node:test';
import {
  DIFFERENTIAL_APP_ID,
  DIFFERENTIAL_SCENARIOS,
  type DivergenceSignature,
} from './scenarios.ts';
import { parseMaestroConformanceSource } from '../harness.ts';
import {
  matchesSignature,
  parseRunnerArgs,
  runScenario,
  selectScenarios,
  validateScenarios,
} from './run.ts';

const CONFORMANCE_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

test('every differential scenario references an existing corpus flow with a unique id', () => {
  assert.doesNotThrow(() => validateScenarios());
  assert.ok(DIFFERENTIAL_SCENARIOS.length > 0, 'expected at least one differential scenario');
});

test('the settle-loop bug class (4) is covered by a differential scenario', () => {
  const settle = DIFFERENTIAL_SCENARIOS.find((scenario) => scenario.bugClass === 4);
  assert.ok(
    settle,
    'bug class 4 (settle ordering) has no reflectable constant; it must be a differential scenario',
  );
  assert.equal(settle?.id, 'settle-after-tap');
});

test('--trace-root is accepted so engine-side invariants can be evaluated', () => {
  const options = parseRunnerArgs(['--trace-root', '/tmp/artifacts']);
  assert.equal(options.traceRoot, '/tmp/artifacts');
});

// The layer-1 corpus exists to be PARSED: its flows name a fictional
// com.example.app and elements that exist on no device. Pointing a device
// scenario at one produces a run that fails before it exercises any runtime
// behavior — which is exactly how the settle detector was silently vacuous.
test('no differential scenario points at the parse-only layer-1 corpus', () => {
  const corpusBacked = DIFFERENTIAL_SCENARIOS.filter((s) => s.flow.startsWith('corpus/'));
  assert.deepEqual(
    corpusBacked.map((s) => s.id),
    [],
    'device scenarios must use differential/flows (real fixture app), not the parse corpus',
  );
  for (const scenario of DIFFERENTIAL_SCENARIOS) {
    assert.ok(
      scenario.flow.startsWith('differential/flows/'),
      `${scenario.id}: expected a device flow under differential/flows`,
    );
  }
});

// A declared divergence without an issue behind it is how "temporarily expected"
// becomes permanent without anyone deciding to. Layer 1 requires `unsupported`
// on every we-reject entry for the same reason; enforce the twin here, because
// the lesson of this whole arc is that prose discipline does not survive contact
// with a two-day debugging session.
test('every knownDivergence carries a tracking issue', () => {
  const problems: string[] = [];
  for (const scenario of DIFFERENTIAL_SCENARIOS) {
    const declared = scenario.knownDivergence;
    if (!declared) continue;
    if (!/^https:\/\/github\.com\/.+\/issues\/\d+$/.test(declared.tracking ?? '')) {
      problems.push(`${scenario.id}: knownDivergence.tracking must be a GitHub issue URL`);
    }
    if (!declared.reason || declared.reason.length < 20) {
      problems.push(`${scenario.id}: knownDivergence.reason must explain what it blocks`);
    }
  }
  assert.deepEqual(problems, [], problems.join('\n'));
});

// A waiver must cover the ONE failure it was granted for. Without an exact
// signature it is blanket amnesty: while a gap is open the job would also
// swallow upstream regressing or a different invariant breaking — hiding the
// next bug behind the last one.
describe('knownDivergence signature matching', () => {
  const sig: DivergenceSignature = {
    maestro: 'pass',
    agentDevice: 'fail',
    invariants: ['no-data'],
  };
  const engine = (outcome: 'pass' | 'fail') => ({
    engine: 'maestro' as const,
    outcome,
    exitCode: outcome === 'pass' ? 0 : 1,
  });
  const inv = (status: 'held' | 'violated' | 'no-data') =>
    ({
      invariant: { kind: 'stepDurationBelow', command: 'tapOn', maxMs: 1, because: 'x' },
      status,
      detail: '',
    }) as never;

  test('matches the declared failure exactly', () => {
    assert.equal(matchesSignature(sig, engine('pass'), engine('fail'), [inv('no-data')]), true);
  });

  test('upstream also failing is NOT covered by the waiver', () => {
    // The #1299 shape is maestro=pass. If Maestro starts failing too, that is a
    // different problem and must not ride in green on this declaration.
    assert.equal(matchesSignature(sig, engine('fail'), engine('fail'), [inv('no-data')]), false);
  });

  test('our engine unexpectedly passing is NOT covered (declaration is stale)', () => {
    assert.equal(matchesSignature(sig, engine('pass'), engine('pass'), [inv('no-data')]), false);
  });

  test('a different invariant outcome is NOT covered', () => {
    assert.equal(matchesSignature(sig, engine('pass'), engine('fail'), [inv('violated')]), false);
  });

  test('a new invariant appearing is NOT covered', () => {
    assert.equal(
      matchesSignature(sig, engine('pass'), engine('fail'), [inv('no-data'), inv('violated')]),
      false,
    );
  });

  test('every declaration states its expected signature', () => {
    for (const scenario of DIFFERENTIAL_SCENARIOS) {
      const declared = scenario.knownDivergence;
      if (!declared) continue;
      assert.ok(
        declared.expected,
        `${scenario.id}: knownDivergence must declare an expected signature`,
      );
      assert.ok(
        ['pass', 'fail'].includes(declared.expected.maestro) &&
          ['pass', 'fail'].includes(declared.expected.agentDevice),
        `${scenario.id}: signature must state both engines' outcomes`,
      );
      // A waiver that expects both engines to pass is not a divergence at all.
      assert.ok(
        !(declared.expected.maestro === 'pass' && declared.expected.agentDevice === 'pass'),
        `${scenario.id}: a signature where both engines pass describes no divergence`,
      );
    }
  });
});

test('an ordinary Maestro process failure cannot satisfy a behavioral waiver', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'maestro-failure-'));
  const maestroCli = path.join(root, 'maestro.mjs');
  const agentDeviceCli = path.join(root, 'agent-device.mjs');
  try {
    fs.writeFileSync(maestroCli, 'process.exit(1);\n');
    fs.writeFileSync(agentDeviceCli, 'process.exit(0);\n');

    const report = runScenario(
      {
        id: 'maestro-infrastructure-failure',
        flow: 'differential/flows/settle-after-tap.yaml',
        comparesAcrossEngines: 'test fixture',
        expect: 'pass',
        divergenceMeans: 'test fixture',
        knownDivergence: {
          reason: 'A behavioral Maestro failure is temporarily accepted for this test fixture.',
          tracking: 'https://github.com/callstack/agent-device/issues/1',
          expected: { maestro: 'fail', agentDevice: 'pass' },
        },
      },
      {
        dryRun: false,
        maestroBin: `${process.execPath} ${maestroCli}`,
        agentDeviceCliArgv: [agentDeviceCli],
      },
    );

    assert.equal(report.maestro.failureKind, 'infrastructure');
    assert.equal(report.status, 'infrastructure-failed');
    assert.equal(report.failed, true);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// The production route the P1 review asked for: environment -> parseRunnerArgs ->
// runScenario -> spawn. Calling runAgentDeviceEngine with a hand-built argv proves
// the spawn, but not that the two variables reach it intact — and it is the
// environment contract that broke the nightly and that a whitespace-split
// AGENT_DEVICE_CLI would break again in the other direction.
describe('the agent-device CLI environment route', () => {
  const ROUTE_SCENARIO = {
    id: 'cli-env-route',
    flow: 'differential/flows/settle-after-tap.yaml',
    comparesAcrossEngines: 'test fixture',
    expect: 'pass',
    divergenceMeans: 'test fixture',
  } as const;

  /** Run one scenario with AGENT_DEVICE_CLI* set, restoring the environment after. */
  function reportForEnvironment(entry: string, nodeFlags: string, maestroCli: string) {
    const previous = {
      entry: process.env.AGENT_DEVICE_CLI,
      flags: process.env.AGENT_DEVICE_CLI_NODE_FLAGS,
    };
    process.env.AGENT_DEVICE_CLI = entry;
    process.env.AGENT_DEVICE_CLI_NODE_FLAGS = nodeFlags;
    try {
      const options = parseRunnerArgs([]);
      return {
        argv: options.agentDeviceCliArgv,
        report: runScenario(ROUTE_SCENARIO, {
          ...options,
          maestroBin: `${process.execPath} ${maestroCli}`,
        }),
      };
    } finally {
      restoreEnv('AGENT_DEVICE_CLI', previous.entry);
      restoreEnv('AGENT_DEVICE_CLI_NODE_FLAGS', previous.flags);
    }
  }

  function restoreEnv(name: string, value: string | undefined): void {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }

  /**
   * `spaced` is a subdirectory whose NAME carries the space, so the entry path
   * holds one whatever the file is called. The maestro stub deliberately stays
   * out of it: `runMaestroEngine` still splits its command on spaces, and a
   * spaced stub path would fail this test for the other engine's reason.
   */
  function withFixtureRoot(run: (spaced: string, maestroCli: string) => void): void {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'agent-device-env-route-'));
    const spaced = path.join(root, 'agent device');
    fs.mkdirSync(spaced);
    const maestroCli = path.join(root, 'maestro.mjs');
    fs.writeFileSync(maestroCli, 'process.exit(0);\n');
    try {
      run(spaced, maestroCli);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  }

  test('a CLI path containing spaces reaches the spawn unsplit', () => {
    withFixtureRoot((spaced, maestroCli) => {
      const entry = path.join(spaced, 'cli.mjs');
      fs.writeFileSync(entry, 'process.exit(0);\n');

      const { argv, report } = reportForEnvironment(entry, '', maestroCli);

      assert.deepEqual(argv, [entry], 'the entry path must stay one argument');
      assert.equal(report.agentDevice.outcome, 'pass');
      assert.equal(report.failed, false);
    });
  });

  test('node flags stay separate arguments alongside a spaced path', () => {
    withFixtureRoot((spaced, maestroCli) => {
      const entry = path.join(spaced, 'cli.ts');
      fs.writeFileSync(entry, 'const code: number = 0;\nprocess.exit(code);\n');

      const { argv, report } = reportForEnvironment(
        entry,
        '--experimental-strip-types',
        maestroCli,
      );

      // Neither property is expressible in a single whitespace-joined variable.
      assert.deepEqual(argv, ['--experimental-strip-types', entry]);
      assert.equal(report.agentDevice.outcome, 'pass');
      assert.equal(report.failed, false);
    });
  });
});

test('every device flow targets the fixture app the workflow installs', () => {
  for (const scenario of DIFFERENTIAL_SCENARIOS) {
    const flowPath = path.join(CONFORMANCE_DIR, scenario.flow);
    const parsed = parseMaestroConformanceSource(fs.readFileSync(flowPath, 'utf8'), flowPath);
    assert.equal(
      parsed.appId,
      DIFFERENTIAL_APP_ID,
      `${scenario.id} must target ${DIFFERENTIAL_APP_ID}; a flow against any other app cannot run on the CI simulator`,
    );
  }
});

test('--only selects a single scenario and rejects unknown ids', () => {
  assert.equal(selectScenarios('settle-after-tap').length, 1);
  assert.throws(() => selectScenarios('does-not-exist'), /No scenario named/);
});

test('runner arg parsing honors dry-run and platform', () => {
  const options = parseRunnerArgs(['--dry-run', '--platform', 'ios']);
  assert.equal(options.dryRun, true);
  assert.equal(options.platform, 'ios');
});
