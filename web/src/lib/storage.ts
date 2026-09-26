/**
 * Browser storage that never throws (private windows, blocked storage) and falls back to memory.
 *
 * What lives where (decision, see README):
 * - Production token pair: localStorage, so every tab shares one login and refreshes it under
 *   a Web Lock. Also held in memory by the store.
 * - Test environment (its app_secret, name) and its token pair: sessionStorage, so a test world
 *   stays in the tab that entered it and never leaks into production tabs.
 * - Preferences (telemetry, theme, last team): localStorage.
 */
const memory = new Map<string, string>();

function area(kind: "local" | "session"): Storage | null {
  try {
    const s = kind === "local" ? window.localStorage : window.sessionStorage;
    const probe = "__bridge_probe__";
    s.setItem(probe, "1");
    s.removeItem(probe);
    return s;
  } catch {
    return null;
  }
}

export function read(kind: "local" | "session", key: string): string | null {
  const s = area(kind);
  try {
    return s ? s.getItem(key) : (memory.get(`${kind}:${key}`) ?? null);
  } catch {
    return memory.get(`${kind}:${key}`) ?? null;
  }
}

export function write(kind: "local" | "session", key: string, value: string): void {
  const s = area(kind);
  try {
    if (s) s.setItem(key, value);
    else memory.set(`${kind}:${key}`, value);
  } catch {
    memory.set(`${kind}:${key}`, value);
  }
}

export function remove(kind: "local" | "session", key: string): void {
  const s = area(kind);
  try {
    s?.removeItem(key);
  } catch {
    /* ignore */
  }
  memory.delete(`${kind}:${key}`);
}

export function readJson<T>(kind: "local" | "session", key: string): T | null {
  const raw = read(kind, key);
  if (!raw) return null;
  try {
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
}

export function writeJson(kind: "local" | "session", key: string, value: unknown): void {
  write(kind, key, JSON.stringify(value));
}
