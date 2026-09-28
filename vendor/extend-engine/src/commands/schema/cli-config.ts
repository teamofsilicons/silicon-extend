import type { CliFlags } from '@agent-device/contracts/command';
import fs from 'node:fs';
import path from 'node:path';
import { AppError } from '@agent-device/kernel/errors';
import { mergeDefinedFlags } from './merge-flags.ts';
import { type FlagKey } from '@agent-device/command-registry/flag-types';
import { projectConfigFlagKeys } from '@agent-device/command-registry/flag-registry';
import { expandUserHomePath, resolveUserPath } from '@agent-device/host-kit/file';
import {
  getConfigurableOptionSpecs,
  getOptionSpec,
  parseOptionValueFromSource,
} from './option-schema.ts';
import { parseInstallSourceConfig } from '@agent-device/provision-kit/install-source-config';
import { RETIRED_SCREENSHOT_MAX_SIZE } from '@agent-device/contracts/capture';
import { type EnvMap } from '@agent-device/kernel/source-value';
import { ENGINE_HOME_DIRECTORY_SEGMENTS } from '@agent-device/kernel/extend-names';

export function resolveConfigBackedFlagDefaults(options: {
  command: string | null;
  cwd: string;
  cliFlags: CliFlags;
  env?: EnvMap;
}): Partial<CliFlags> {
  const env = options.env ?? process.env;
  const defaults = mergeDefinedFlags(
    {} as Partial<CliFlags>,
    loadConfigFileDefaults(resolveConfigPaths(options.cwd, options.cliFlags.config, env)),
  );
  return mergeDefinedFlags(defaults, readEnvFlagDefaults(env, options.command));
}

type ConfigFileSource = 'user' | 'project' | 'explicit';

type ConfigPath = { path: string; required: boolean; source: ConfigFileSource };

// Project config is repository-controlled, so a flag stays operator-only unless its
// own declaration sets `projectConfig: true`. This set derives from those declarations;
// admitting a new key to ./extend-engine.json edits the declaration, not this file.
const PROJECT_CONFIG_FLAG_KEYS = projectConfigFlagKeys();

function resolveConfigPaths(
  cwd: string,
  explicitCliConfigPath: string | undefined,
  env: EnvMap,
): ConfigPath[] {
  const explicitConfig =
    explicitCliConfigPath ?? (env.EXTEND_ENGINE_CONFIG?.trim() || env.AGENT_DEVICE_CONFIG);
  if (explicitConfig) {
    return [
      { path: resolveInputPath(explicitConfig, cwd, env), required: true, source: 'explicit' },
    ];
  }
  return [
    { path: resolveUserConfigPath(env), required: false, source: 'user' },
    { path: path.resolve(cwd, PROJECT_CONFIG_FILE_NAME), required: false, source: 'project' },
  ];
}

// Silicon Extend's names for the engine's config files. The fork's upstream names
// (~/.agent-device/config.json, ./agent-device.json) are not read: they belong to a standalone
// upstream install, whose settings must not leak into the engine Extend runs.
const PROJECT_CONFIG_FILE_NAME = 'extend-engine.json';

function resolveUserConfigPath(env: EnvMap): string {
  return path.join(
    expandUserHomePath('~', { env }),
    ...ENGINE_HOME_DIRECTORY_SEGMENTS,
    'config.json',
  );
}

function resolveInputPath(inputPath: string, cwd: string, env: EnvMap): string {
  return resolveUserPath(inputPath, { cwd, env });
}

function loadConfigFileDefaults(pathsToCheck: ConfigPath[]): Partial<CliFlags> {
  const merged: Partial<CliFlags> = {};
  for (const entry of pathsToCheck) {
    const parsed = loadSingleConfigFile(entry);
    mergeDefinedFlags(merged, parsed);
  }
  return merged;
}

function loadSingleConfigFile(entry: ConfigPath): Partial<CliFlags> {
  const { path: filePath, required } = entry;
  if (!fs.existsSync(filePath)) {
    if (required) {
      throw new AppError('INVALID_ARGS', `Config file not found: ${filePath}`);
    }
    return {};
  }

  let raw: string;
  try {
    raw = fs.readFileSync(filePath, 'utf8');
  } catch (error) {
    throw new AppError('INVALID_ARGS', `Failed to read config file: ${filePath}`, {
      cause: error instanceof Error ? error.message : String(error),
    });
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new AppError('INVALID_ARGS', `Invalid JSON in config file: ${filePath}`, {
      cause: error instanceof Error ? error.message : String(error),
    });
  }

  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new AppError('INVALID_ARGS', `Config file must contain a JSON object: ${filePath}`);
  }

  return parseConfigObject(parsed as Record<string, unknown>, {
    source: entry.source,
    label: `${entry.source === 'project' ? 'project ' : ''}config file ${filePath}`,
  });
}

function parseConfigObject(
  source: Record<string, unknown>,
  origin: { source: ConfigFileSource; label: string },
): Partial<CliFlags> {
  const flags: Partial<CliFlags> = {};
  for (const [rawKey, rawValue] of Object.entries(source)) {
    const key = rawKey as FlagKey;
    const spec = getOptionSpec(key);
    if (!spec) {
      if (rawKey === RETIRED_SCREENSHOT_MAX_SIZE.flagKey) {
        throw new AppError(
          'INVALID_ARGS',
          `Config key "${rawKey}" in ${origin.label} was removed; use "screenshotScale" (0.01-1) to downscale screenshots. Recordings capture at native resolution.`,
        );
      }
      throw new AppError('INVALID_ARGS', `Unknown config key "${rawKey}" in ${origin.label}.`);
    }
    if (!spec.configurable) {
      throw new AppError(
        'INVALID_ARGS',
        `Config key "${rawKey}" is not allowed in ${origin.label}. This key is not supported in config files.`,
      );
    }
    if (origin.source === 'project' && !PROJECT_CONFIG_FLAG_KEYS.has(key)) {
      throw new AppError(
        'INVALID_ARGS',
        `Config key "${rawKey}" is not allowed in ${origin.label}. Move it to ~/.silicon-extend/engine/config.json, pass it with --config or EXTEND_ENGINE_CONFIG, or provide it through CLI flags/environment variables.`,
      );
    }
    if (key === 'installSource') {
      flags.installSource = parseInstallSourceConfig(rawValue, origin.label);
      continue;
    }
    (flags as Record<string, unknown>)[key] = parseOptionValueFromSource(
      spec,
      rawValue,
      origin.label,
      rawKey,
    );
  }
  return flags;
}

// Commands that honored EXTEND_ENGINE_SCREENSHOT_MAX_SIZE (AGENT_DEVICE_SCREENSHOT_MAX_SIZE) in released versions.
// A stale env var must fail closed for them (sizing must not silently vanish)
// while every other command keeps working.
const RETIRED_MAX_SIZE_ENV_COMMANDS = new Set(['screenshot', 'record']);

function readEnvFlagDefaults(env: EnvMap, command: string | null): Partial<CliFlags> {
  const retiredEnvVar = [
    RETIRED_SCREENSHOT_MAX_SIZE.envVar,
    RETIRED_SCREENSHOT_MAX_SIZE.legacyEnvVar,
  ].find((name) => {
    const value = env[name];
    return typeof value === 'string' && value.trim().length > 0;
  });
  if (command !== null && RETIRED_MAX_SIZE_ENV_COMMANDS.has(command) && retiredEnvVar) {
    throw new AppError(
      'INVALID_ARGS',
      `${retiredEnvVar} was removed. ${RETIRED_SCREENSHOT_MAX_SIZE.migration[command === 'record' ? 'record' : 'screenshot']}`,
    );
  }
  const flags: Partial<CliFlags> = {};
  for (const spec of getConfigurableOptionSpecs(command)) {
    if (spec.key === 'installSource') continue;
    const envNames = spec.env.names;
    const envValue = envNames
      .map((name) => ({ name, value: env[name] }))
      .find((entry) => typeof entry.value === 'string' && entry.value.trim().length > 0);
    if (!envValue) continue;
    (flags as Record<string, unknown>)[spec.key] = parseOptionValueFromSource(
      spec,
      envValue.value as string,
      `environment variable ${envValue.name}`,
      envValue.name,
    );
  }
  return flags;
}
