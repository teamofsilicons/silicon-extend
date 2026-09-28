// Where Silicon Extend's device engine keeps its files under a home folder: `~/.silicon-extend/engine`
// (the fork's upstream used `~/.agent-device`). The default state directory, the user config file,
// device claims, logs, the Apple runner's builds and leases, and the helpers the engine builds all
// live under it, so a Carbon who finds the folder can tell what made it.
export const ENGINE_HOME_DIRECTORY_SEGMENTS = ['.silicon-extend', 'engine'] as const;

/** The same folder as the engine names it in help and advice. */
export const ENGINE_HOME_DISPLAY_PATH = '~/.silicon-extend/engine';

// A project the engine runs in gets the same folder, relative to the project root, for what the
// engine writes there (Metro logs, companion state, `test` artifacts).
export const ENGINE_PROJECT_DIRECTORY_SEGMENTS = ENGINE_HOME_DIRECTORY_SEGMENTS;
