import { expect, test, vi } from 'vitest';
import { createCloudWebDriverCapabilities } from './capabilities.ts';
import type { DeviceInfo } from '@agent-device/kernel/device';
import { createWebDriverDeploymentRuntime } from './runtime-deployment.ts';
import type { WebDriverProviderSession } from './runtime-session.ts';
import type { CloudWebDriverUploadApp } from './runtime.ts';

const device: DeviceInfo = {
  platform: 'android',
  id: 'webdriver:stale',
  name: 'Stale WebDriver device',
  kind: 'device',
  target: 'mobile',
  booted: true,
};

test('keeps a stale WebDriver owner unavailable before any deployment attempt', () => {
  const deployment = createWebDriverDeploymentRuntime({
    provider: 'webdriver-test',
    findSessionForDevice: () => undefined,
  });

  expect(deployment.fact(device)).toMatchObject({
    available: false,
    reason: 'owner-capability-missing',
  });
});

test('aborts a WebDriver provider deployment while its upload is in flight', async () => {
  const controller = new AbortController();
  const abortReason = new Error('request cancelled during provider upload');
  const installApp = vi.fn(async () => undefined);
  const uploadApp: CloudWebDriverUploadApp = vi.fn(async (params) => {
    expect(params.signal).toBe(controller.signal);
    return await rejectWhenAborted(params.signal);
  });
  const deployment = createWebDriverDeploymentRuntime({
    provider: 'webdriver-test',
    uploadApp,
    findSessionForDevice: () => activeSession(installApp),
  });

  const pending = deployment.deployApp(
    device,
    { app: 'com.example.app', appPath: '/tmp/App.apk', replaceExisting: false },
    controller.signal,
  );
  controller.abort(abortReason);

  await expect(pending).rejects.toBe(abortReason);
  expect(installApp).not.toHaveBeenCalled();
});

test('aborts a WebDriver provider deployment while its install is in flight', async () => {
  const controller = new AbortController();
  const abortReason = new Error('request cancelled during provider install');
  const installApp = vi.fn(async (_appPath: string, signal?: AbortSignal) => {
    expect(signal).toBe(controller.signal);
    return await rejectWhenAborted(signal);
  });
  const deployment = createWebDriverDeploymentRuntime({
    provider: 'webdriver-test',
    uploadApp: async () => ({ appReference: 'bs://uploaded-app' }),
    findSessionForDevice: () => activeSession(installApp),
  });

  const pending = deployment.deployApp(
    device,
    { app: 'com.example.app', appPath: '/tmp/App.apk', replaceExisting: false },
    controller.signal,
  );
  controller.abort(abortReason);

  await expect(pending).rejects.toBe(abortReason);
  expect(installApp).toHaveBeenCalledWith('bs://uploaded-app', controller.signal);
});

function activeSession(
  installApp: (appPath: string, signal?: AbortSignal) => Promise<void>,
): WebDriverProviderSession {
  return {
    capabilities: createCloudWebDriverCapabilities({
      provider: 'webdriver-test',
      platform: 'android',
    }),
    client: { installApp },
    prepared: {},
  } as unknown as WebDriverProviderSession;
}

async function rejectWhenAborted(signal: AbortSignal | undefined): Promise<never> {
  return await new Promise<never>((_resolve, reject) => {
    if (!signal) {
      reject(new Error('provider operation was not given the binding signal'));
      return;
    }
    if (signal.aborted) {
      reject(signal.reason);
      return;
    }
    signal.addEventListener('abort', () => reject(signal.reason), { once: true });
  });
}
