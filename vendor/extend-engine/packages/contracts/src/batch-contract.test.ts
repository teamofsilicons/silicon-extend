import { expect, test } from 'vitest';
import { AppError } from '@agent-device/kernel/errors';
import {
  BATCH_STEP_SHAPE_HINT,
  readBatchStepInputObject,
  readBatchStepRecord,
} from './batch-contract.ts';

function hintOf(read: () => unknown): string | undefined {
  try {
    read();
  } catch (error) {
    return error instanceof AppError ? error.details?.hint : undefined;
  }
  throw new Error('Expected the read to refuse');
}

// This module validates batch steps for the Node client and the MCP tools as well as the CLI, so
// a terminal recovery step here would be unrunnable advice on two of the three surfaces (#2062).
test('the shared step-shape hint names the shape without naming a surface', () => {
  expect(BATCH_STEP_SHAPE_HINT).toContain('{"command":"<name>","input":{...}}');
  expect(BATCH_STEP_SHAPE_HINT).not.toMatch(/agent-device |--\w/);
});

test('shape refusals carry the shared hint, or the caller-supplied one', () => {
  expect(hintOf(() => readBatchStepRecord('press @e12', 1))).toBe(BATCH_STEP_SHAPE_HINT);
  expect(hintOf(() => readBatchStepInputObject({ command: 'press' }, 1))).toBe(
    BATCH_STEP_SHAPE_HINT,
  );
  expect(hintOf(() => readBatchStepRecord('press @e12', 1, 'surface hint'))).toBe('surface hint');
  expect(hintOf(() => readBatchStepInputObject({ command: 'press' }, 1, 'surface hint'))).toBe(
    'surface hint',
  );
});
