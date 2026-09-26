import { onCleanup, onMount } from "solid-js";

/**
 * Runs `tick` every `ms` while the tab is visible, and right away when it becomes visible again.
 * Ticks never overlap: a slow request delays the next one instead of piling up.
 */
export function usePoll(tick: () => Promise<unknown> | unknown, ms: number, enabled: () => boolean = () => true) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  let disposed = false;

  const schedule = () => {
    clearTimeout(timer);
    if (disposed) return;
    timer = setTimeout(run, ms);
  };
  const run = async () => {
    if (disposed) return;
    if (running || document.visibilityState !== "visible" || !enabled()) return schedule();
    running = true;
    try {
      await tick();
    } catch {
      /* the page shows its own error state */
    } finally {
      running = false;
      schedule();
    }
  };
  const onVisible = () => {
    if (document.visibilityState === "visible") run();
  };

  onMount(() => {
    schedule();
    document.addEventListener("visibilitychange", onVisible);
  });
  onCleanup(() => {
    disposed = true;
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", onVisible);
  });
}
