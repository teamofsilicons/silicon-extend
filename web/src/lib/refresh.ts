import { createRoot, createSignal } from "solid-js";

/**
 * The device list and a device's page are on screen together. When the page changes a device
 * (rename, stop, access, removal) it calls `devicesChanged()` so the list re-reads at once instead
 * of on its next poll.
 */
const [tick, setTick] = createRoot(() => createSignal(0));

export const devicesTick = tick;
export function devicesChanged() {
  setTick((n) => n + 1);
}
