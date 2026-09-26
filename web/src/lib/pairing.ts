/**
 * Pairing codes are 6 hexadecimal characters (TECHNICAL.md §1), shown uppercase and accepted in
 * any case. People copy them as `4F9-C2A`, `4f9 c2a` or `4F9C2A`, so separators are dropped.
 * O is read as 0 and I or L as 1, since none of those letters is hexadecimal and they are what
 * a code is most often misread as.
 */
export interface NormalizedCode {
  /** Uppercase, separators removed, look-alikes mapped: what is sent. */
  code: string;
  valid: boolean;
  /** Why it isn't valid yet, or null. */
  problem: string | null;
}

const SEPARATORS = /[\s\-_.·–—]/g;
const LOOKALIKES: Record<string, string> = { O: "0", I: "1", L: "1" };

export function normalizePairingCode(input: string): NormalizedCode {
  const stripped = input.replace(SEPARATORS, "").toUpperCase();
  const code = stripped.replace(/[OIL]/g, (c) => LOOKALIKES[c] ?? c);
  const bad = [...new Set(code.replace(/[0-9A-F]/g, ""))];
  if (bad.length)
    return {
      code,
      valid: false,
      problem: `A pairing code has only 0–9 and A–F. Remove ${bad.map((c) => `“${c}”`).join(", ")}.`,
    };
  if (code.length < 6)
    return { code, valid: false, problem: code.length ? `${6 - code.length} more character${code.length === 5 ? "" : "s"} to go.` : null };
  if (code.length > 6)
    return { code, valid: false, problem: `A pairing code is 6 characters; this has ${code.length}.` };
  return { code, valid: true, problem: null };
}

/** The 6-character code as the Extend app shows it, in two groups of three for reading aloud. */
export function displayPairingCode(code: string): string {
  const { code: clean } = normalizePairingCode(code);
  return clean.length > 3 ? `${clean.slice(0, 3)} ${clean.slice(3)}` : clean;
}

/** Keeps what the Carbon typed readable while normalising it: uppercase, look-alikes fixed. */
export function formatCodeInput(input: string): string {
  return input
    .toUpperCase()
    .replace(/[OIL]/g, (c) => LOOKALIKES[c] ?? c)
    .slice(0, 12);
}

/** An Apple TV setup code: exactly 4 digits. */
export function normalizeSetupCode(input: string): { code: string; valid: boolean } {
  const code = input.replace(/\D/g, "").slice(0, 4);
  return { code, valid: /^[0-9]{4}$/.test(code) };
}

/** `si:` ids as IAM issues them: `si:` plus a handle. Accepts several, split by spaces or commas. */
export const SILICON_ID = /^si:[a-z0-9][a-z0-9._-]{0,127}$/;

export function parseSiliconIds(input: string): { ids: string[]; invalid: string[] } {
  const parts = input
    .split(/[\s,;]+/)
    .map((p) => p.trim())
    .filter(Boolean)
    .map((p) => (p.startsWith("si:") || p.includes(":") ? p : `si:${p}`))
    .map((p) => p.toLowerCase());
  const ids: string[] = [];
  const invalid: string[] = [];
  for (const p of parts) {
    if (SILICON_ID.test(p)) {
      if (!ids.includes(p)) ids.push(p);
    } else invalid.push(p);
  }
  return { ids, invalid };
}
