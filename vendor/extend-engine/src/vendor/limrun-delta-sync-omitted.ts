throw new Error(
  'The device engine omits @limrun/xdelta3-wasm, which Limrun loads only from client.syncApp. Restore it in tsdown.config.ts to use folder delta sync.',
);
