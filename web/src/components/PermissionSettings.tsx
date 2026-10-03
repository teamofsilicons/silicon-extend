import { approvalPopup, approvalUrl, awaitApproval } from "../lib/approval-popup";
import { randomState } from "../lib/auth";
import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { toApiError, type ApiError, type FeaturePermission, type FeaturePermissionRequest } from "../lib/api";
import { session } from "../lib/session";
import { Button, ErrorNote, Spinner } from "./ui";

const FEATURES = [
  { id: "store", name: "Store files in Briefcase", audience: "briefcase", endpoints: ["briefcase.uploads.reserve", "briefcase.uploads.commit", "briefcase.uploads.status"] },
  { id: "read", name: "Read files from Briefcase", audience: "briefcase", endpoints: ["briefcase.files.read"] },
  { id: "share", name: "Share Briefcase files", audience: "briefcase", endpoints: ["briefcase.invitations.create"] },
  { id: "trash", name: "Move Briefcase files to trash", audience: "briefcase", endpoints: ["briefcase.entries.trash"] },
  { id: "notify", name: "Send and receive Ting notifications", audience: "ting", endpoints: ["tings.send", "subscriptions.register"] },
];
const ACTION_NAMES: Record<string, string> = {
  "briefcase.uploads.reserve": "Prepare file uploads",
  "briefcase.uploads.commit": "Save uploaded files",
  "briefcase.uploads.status": "Check upload progress",
  "briefcase.files.read": "Read files",
  "briefcase.invitations.create": "Share files",
  "briefcase.entries.trash": "Move files to trash",
  "tings.send": "Send notifications",
  "subscriptions.register": "Receive notifications",
};

/** Approval stays with the selected account, team and environment; actions are retried separately. */
export function PermissionSettings() {
  const s = session();
  const client = s.client();
  const [rows, setRows] = createSignal<FeaturePermission[] | null>(null);
  const [selected, setSelected] = createSignal("store");
  const [pending, setPending] = createSignal<FeaturePermissionRequest | null>(null);
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [message, setMessage] = createSignal("");
  let generation = 0;
  let flow = 0;
  let startKey: string | null = null;
  let popupState: string | null = null;
  let popupAbort: AbortController | null = null;
  let completion: { code: string; key: string } | null = null;
  let savedRequest: FeaturePermissionRequest | null = null;

  async function load(current: number) {
    try {
      const result = await client.permissions();
      if (current === generation) setRows(result);
    } catch (e) {
      if (current === generation) setError(toApiError(e));
    }
  }
  createEffect(on([() => s.member()?.type, () => s.member()?.id, s.team, s.world, s.contextRevision], () => {
    const current = ++generation;
    ++flow;
    setPending(null); setCode(""); setRows(null); setError(null); setMessage(""); setBusy(false);
    startKey = null; popupState = null; popupAbort?.abort(); popupAbort = null; completion = null; savedRequest = null;
    if (s.member() && s.team()) void load(current);
  }));
  onCleanup(() => { generation++; flow++; popupAbort?.abort(); completion = null; });

  // Cancelling the local wait cannot revoke IAM approval or a valid existing grant.
  // Keep its request/code/key so an explicit retry recovers the same operation.
  function stopWaiting() {
    ++flow;
    popupAbort?.abort(); popupAbort = null;
    setBusy(false);
  }
  function cancel() {
    stopWaiting(); setPending(null); setError(null);
    setMessage("Approval paused. Your feature choice and retry are saved; no feature action was started.");
  }
  function useManual() {
    stopWaiting(); setError(null);
  }
  function newRequest() {
    stopWaiting();
    savedRequest = null; completion = null; startKey = null; popupState = null;
    setPending(null); setCode("");
    void start();
  }
  function approved(result: FeaturePermission[]) {
    setRows(result); setPending(null); setCode("");
    startKey = null; popupState = null; completion = null; savedRequest = null;
    setMessage("Access approved. Return to the feature and retry your action. You can revoke access in IAM at any time.");
  }

  async function start() {
    stopWaiting();
    const current = flow;
    const feature = FEATURES.find((f) => f.id === selected())!;
    const controller = new AbortController();
    popupAbort = controller;
    let popup: Window | null = null;
    startKey ??= crypto.randomUUID(); popupState ??= randomState();
    setBusy(true); setError(null); setMessage("");
    try {
      // A blocked popup still creates the same bound request for manual recovery.
      popup = approvalPopup();
      controller.signal.addEventListener("abort", () => popup?.close(), { once: true });
      const request = savedRequest ?? await client.requestPermissions(feature.endpoints.map((endpoint_id) => ({ audience: feature.audience, endpoint_id })), startKey, { redirect_uri: new URL("/auth/obo/callback", location.origin).href, state: popupState });
      if (current !== flow) { popup?.close(); return; }
      if (request.state !== popupState) throw new Error("This approval did not match its original request. Start again.");
      approvalUrl(request.consent_url);
      savedRequest = request; setPending(request); setBusy(false);
      if (!popup) {
        setMessage("The popup was blocked. Review access in a new tab, then paste the approval code here.");
        return;
      }
      await awaitApproval(popup, request.consent_url, request.state, async value => {
        if (current !== flow) throw new Error("This approval was cancelled or the account changed. Resume it explicitly.");
        setCode(value);
        completion ??= { code: value, key: crypto.randomUUID() };
        if (completion.code !== value) throw new Error("Retry the original approval code.");
        setBusy(true);
        const result = await client.completePermissions(request.id, value, completion.key, request.state);
        if (current === flow) approved(result);
      }, controller.signal);
    } catch (e) {
      popup?.close(); if (current === flow) setError(toApiError(e));
    } finally {
      if (current === flow) { setBusy(false); popupAbort = null; }
    }
  }

  async function complete(event: Event) {
    event.preventDefault();
    const request = pending();
    const value = code().trim();
    if (!request || !value || value.length > 16384) return;
    stopWaiting();
    const current = flow;
    if (completion && completion.code !== value) {
      setError(toApiError(new Error("Retry the original approval code, or choose a new feature request.")));
      return;
    }
    completion ??= { code: value, key: crypto.randomUUID() };
    setBusy(true); setError(null);
    try {
      const result = await client.completePermissions(request.id, value, completion.key, request.state);
      if (current === flow) approved(result);
    } catch (e) {
      if (current === flow) setError(toApiError(e));
    } finally {
      if (current === flow) setBusy(false);
    }
  }

  return (
    <div class="card" id="feature-permissions" data-testid="settings-permissions">
      <h2 class="card-title">Feature access.</h2>
      <p class="fine">Choose what Extend can do for <strong>{s.member()?.id}</strong> in <strong>{s.team()}</strong>. Review each request in IAM, including the account and organization used by each app.</p>
      <Show when={rows()} fallback={<Show when={!error() && s.team()}><Spinner inline label="Loading approved access…" /></Show>}>
        {(items) => <Show when={items().length} fallback={<p class="muted">No feature access approved for this account and organization yet.</p>}>
          <ul><For each={items()}>{(item) => <li><strong>{item.audience === "briefcase" ? "Briefcase" : item.audience === "ting" ? "Ting" : item.audience}</strong> · {ACTION_NAMES[item.endpoint_id] ?? item.endpoint_id}<br /><span class="fine">{item.actor.public_id ?? item.actor.id ?? "Approved account"} · {item.org_id}</span></li>}</For></ul>
        </Show>}
      </Show>
      <Show when={message()}><p role="status">{message()}</p></Show>
      <Show when={!pending()} fallback={
        <form onSubmit={complete} class="testing-form">
          <p><a class="button primary" href={pending()?.consent_url} target="_blank" rel="noopener noreferrer" onClick={useManual}>Review access in IAM</a></p>
          <p class="fine">Approve using this account, then paste the single-use code below. Approval does not run the original action.</p>
          <label for="feature-code">IAM approval code</label>
          <div class="input-row">
            <input id="feature-code" type="password" autocomplete="off" spellcheck={false} maxlength={16384} value={code()} onInput={(e) => setCode(e.currentTarget.value)} required />
            <Button type="submit" busy={busy()} disabled={!code().trim()}>Save approval</Button>
          </div>
          <Button type="button" variant="ghost" onClick={cancel}>Cancel</Button>
          <Button type="button" variant="ghost" onClick={newRequest}>Start a new request</Button>
        </form>
      }>
        <label for="feature-choice">Feature</label>
        <div class="input-row">
          <select id="feature-choice" value={selected()} disabled={busy()} onChange={(e) => { stopWaiting(); setSelected(e.currentTarget.value); startKey = null; popupState = null; savedRequest = null; completion = null; setCode(""); setError(null); setMessage(""); }}>
            <For each={FEATURES}>{(feature) => <option value={feature.id}>{feature.name}</option>}</For>
          </select>
          <Button onClick={start} busy={busy()} disabled={!s.team()}>Request access</Button>
          <Show when={busy()}><Button type="button" variant="ghost" onClick={cancel}>Cancel</Button></Show>
        </div>
      </Show>
      <Show when={selected() === "notify"}><p class="fine">For notifications, choose this account and the device organization in IAM. Then turn on notifications below.</p></Show>
      <ErrorNote error={error()} compact />
    </div>
  );
}
