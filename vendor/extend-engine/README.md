# Silicon Extend device engine

The device engine reads a screen as a list of things to act on (buttons, fields, lists) and acts
on them. Silicon Extend runs it on Mac and Linux computers, and on a Mac for the iPhones and iPads
it hosts. Silicons reach it through `extend <command>`; they never run it directly.

It is a fork of an MIT-licensed project. [`FORK.md`](FORK.md) names the upstream and lists every
change Silicon Extend made, newest first; [`LICENSE`](LICENSE) is the upstream licence, which
ships with the engine.

## Build and test

Node.js 22.12 or newer and pnpm (the version in `package.json`'s `packageManager`).

```bash
cd vendor/extend-engine
pnpm install --frozen-lockfile
pnpm build        # dist/, which bin/extend-engine.mjs runs
pnpm typecheck
pnpm exec vitest run --project unit-core <test files>
```

Silicon Extend's CI runs every test file an Extend change touched; the list is the `fork` job in
`.github/workflows/ci.yml` at the repository root. Packaging for the desktop app is in
`apps/desktop` (`README.md` there).

## Settings

The engine's settings are environment variables named `EXTEND_ENGINE_<X>` (for example
`EXTEND_ENGINE_STATE_DIR`). The engine's code reads the fork's internal `AGENT_DEVICE_<X>` names:
every entry point imports `src/extend-env.ts` first, which copies each set `EXTEND_ENGINE_<X>` onto
`AGENT_DEVICE_<X>`. The new name wins, and an old name set alone still works.

## Layout

| Path | What it is |
| --- | --- |
| `bin/extend-engine.mjs` | The command-line entry (packaging puts Extend's `runtime-entry.mjs` here and moves this one to `bin/extend-engine-cli.mjs`) |
| `src/`, `packages/` | The engine: CLI, daemon and platform runtimes |
| `apple/` | The macOS helper and the iPhone/iPad helper (XCTest runner) sources, built on the Mac |
| `android/` | The engine's Android helpers |
| `linux/` | The AT-SPI reader and the X11 screen recorder (Python) |
| `contracts/` | Fixtures shared by the TypeScript and Swift sides |
| `docs/` | Architecture decisions (`docs/adr/`) and procedures for changing the engine (`docs/agents/`) |
| `AGENTS.md`, `CONTEXT.md` | Guidance and vocabulary for working on the engine |
