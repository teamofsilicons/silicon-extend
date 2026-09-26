import type {
  ApplicationLifecycleRuntimeOperations,
  LocalApplicationInteractorHost,
} from '@agent-device/contracts/application-lifecycle-runtime';
import {
  bindDirectApplicationLifecycle,
  bindLocalApplicationLifecycleInteractor,
} from '@agent-device/contracts/application-lifecycle-interaction';
import { isDeepLinkTarget } from '@agent-device/contracts/command';
import type { DeviceInfo } from '@agent-device/kernel/device';

type LinuxLifecycleParams = Readonly<{
  host: LocalApplicationInteractorHost;
  device: DeviceInfo;
  signal: AbortSignal;
}>;

/** Linux owns its desktop lifecycle behavior after the local owner is bound. */
export function bindLinuxApplicationLifecycle(
  params: LinuxLifecycleParams,
): ApplicationLifecycleRuntimeOperations {
  const operations = bindDirectApplicationLifecycle({
    owner: 'Linux',
    openTargetIdentity: 'app-name',
    binding: bindLocalApplicationLifecycleInteractor({
      device: params.device,
      signal: params.signal,
      resolveInteractor: params.host.resolve,
    }),
  });
  return Object.freeze({
    ...operations,
    resolveOpenTarget: async (input) => {
      const resolved = await operations.resolveOpenTarget(input);
      const target = input.target?.trim();
      return {
        ...resolved,
        ...(target && !isDeepLinkTarget(target) ? { appBundleId: target } : {}),
      };
    },
  });
}
