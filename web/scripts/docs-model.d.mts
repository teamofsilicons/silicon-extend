// Types for docs-model.mjs, so the unit tests and the docs page share one shape.

/** A command's errors: error codes, a sentence, or both. */
export interface CommandErrors {
  codes: string[];
  note: string | null;
}

type Json = string | number | boolean | null | Json[] | { [key: string]: Json };

export interface DocsCommand {
  usage: string;
  who: string | null;
  needs: Json;
  capability: string | null;
  summary: string | null;
  takes: Json;
  gives: Json;
  errors: CommandErrors | null;
  api: string | null;
  notes: string | null;
  device: boolean;
}

export function commandErrors(value: unknown, usage: string): CommandErrors | null;
export function commandText(value: unknown, usage: string, field: string, separator?: string): string | null;
export function normalizeCommand(c: Record<string, unknown>, device: boolean): DocsCommand;
export function errorTable(errors: unknown): { code: string; exit: number | null; meaning: string }[];
