import { createSignal, For, onMount, Show } from "solid-js";
import { session } from "../lib/session";
import { toApiError, type ApiError, type ImportableDevice } from "../lib/api";
import { Button, ErrorNote, Modal, Spinner } from "./ui";
import { devicesChanged } from "../lib/refresh";
import { OS_LABEL } from "../config";

export default function ImportDevices(props: { close: () => void }) {
  const s = session(),
    client = s.client(),
    organization = s.team();
  const [items, setItems] = createSignal<ImportableDevice[]>(),
    [error, setError] = createSignal<ApiError | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null),
    [shared, setShared] = createSignal(true);
  const keys = new Map<string, string>();
  onMount(async () => {
    try {
      const all: ImportableDevice[] = [];
      let cursor: string | null = null;
      do {
        const page = await client.importableDevices(cursor);
        all.push(...page.items);
        cursor = page.next_cursor;
      } while (cursor);
      setItems(all);
    } catch (e) {
      setError(toApiError(e));
    }
  });
  async function add(device: ImportableDevice) {
    setBusy(device.device_id);
    setError(null);
    const visibility = shared() ? "team" : "personal",
      request = `${device.device_id}:${visibility}`;
    if (!keys.has(request)) keys.set(request, crypto.randomUUID());
    try {
      await client.importDevice(device.device_id, visibility, keys.get(request)!);
      setItems(items()?.filter((d) => d.device_id !== device.device_id));
      devicesChanged();
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(null);
    }
  }
  return (
    <Modal open title="Import your devices" onClose={props.close}>
      <p>
        Add a device you already configured to <strong>{organization}</strong>. Its setup stays connected.
      </p>
      <label class="checkbox-row">
        <input type="checkbox" checked={shared()} disabled={!!busy()} onChange={(e) => setShared(e.currentTarget.checked)} />
        Visible to organization members
      </label>
      <p class="fine">
        Off hides imported devices from other Carbons and Silicons without access. Silicons you explicitly grant access in this organization can still see and use them.
      </p>
      <ErrorNote error={error()} />
      <Show
        when={items()}
        fallback={
          <Show when={!error()}>
            <Spinner label="Finding your devices…" />
          </Show>
        }
      >
        {(list) => (
          <Show
            when={list().length}
            fallback={<p class="fine">All your configured devices are already here. You can also add a new device.</p>}
          >
            <For each={list()}>
              {(d) => (
                <div class="import-device-row">
                  <div>
                    <strong>{d.name}</strong>
                    <p class="fine">
                      {OS_LABEL[d.os]}
                      {d.model ? ` · ${d.model}` : ""}
                    </p>
                  </div>
                  <Button disabled={!!busy()} busy={busy() === d.device_id} onClick={() => void add(d)}>
                    Import
                  </Button>
                </div>
              )}
            </For>
          </Show>
        )}
      </Show>
    </Modal>
  );
}
