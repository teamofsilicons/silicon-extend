const TYPE = "silicon:feature-approval";
export function approvalPopup(): Window | null {
  try {
    return window.open("about:blank", `feature-approval-${crypto.randomUUID()}`, "popup,width=580,height=760");
  } catch {
    return null;
  }
}
export function validApprovalMessage(event: Pick<MessageEvent, "origin" | "source" | "data">, origin: string, popup: Window, state: string): boolean {
  return event.origin === origin && event.source === popup && event.data?.type === TYPE && event.data.state === state &&
    (event.data.error === "access_denied" || (typeof event.data.code === "string" && /^obc_[^\s\x00-\x1f\x7f]{1,16380}$/.test(event.data.code)));
}
export function approvalUrl(consentUrl: string): URL {
  const url = new URL(consentUrl);
  if (!(url.protocol === "https:" || (url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))) || url.username || url.password || url.hash) throw new Error("The approval link is invalid.");
  return url;
}
export function awaitApproval(popup: Window, consentUrl: string, state: string, complete: (code: string) => Promise<void>, signal?: AbortSignal): Promise<void> {
  let url: URL;
  try { url = approvalUrl(consentUrl); } catch (error) { popup.close(); throw error; }
  url.searchParams.set("display", "popup");
  return new Promise((resolve, reject) => {
    let completing = false, settled = false;
    const cleanup = () => { window.removeEventListener("message", receive); signal?.removeEventListener("abort", cancel); clearInterval(closed); clearTimeout(timeout); popup.close(); };
    const fail = (error: unknown) => { if (settled) return; settled = true; cleanup(); reject(error); };
    const cancel = () => fail(new Error("Approval cancelled. No feature action was started."));
    const receive = async (event: MessageEvent) => {
      if (settled || completing || !validApprovalMessage(event, location.origin, popup, state)) return;
      if (event.data.error) { fail(new Error("Access was not approved.")); return; }
      completing = true;
      try {
        await complete(event.data.code);
        if (settled) return;
        popup.postMessage({ type: TYPE + ":complete", state }, location.origin);
        settled = true;
        cleanup(); resolve();
      } catch (error) { fail(error); }
    };
    const closed = setInterval(() => { if (popup.closed && !completing) cancel(); }, 500);
    const timeout = setTimeout(cancel, 15 * 60 * 1000);
    window.addEventListener("message", receive); signal?.addEventListener("abort", cancel, { once: true });
    if (signal?.aborted) { cancel(); return; }
    popup.location.href = url.href; popup.focus();
  });
}
/** One-use code handoff only. The authenticated opener completes the server request before acknowledgement. */
export function deliverApproval(params: URLSearchParams): boolean {
  const opener = window.opener, state = params.get("state");
  if (!opener || !state || state.length < 32 || state.length > 512) return false;
  const payload = { type: TYPE, state, code: params.get("code"), error: params.get("error") };
  if (!validApprovalMessage({ origin: location.origin, source: opener, data: payload }, location.origin, opener, state)) return false;
  const receive = (event: MessageEvent) => {
    if (event.origin === location.origin && event.source === opener && event.data?.type === TYPE + ":complete" && event.data.state === state) { window.removeEventListener("message", receive); window.close(); }
  };
  window.addEventListener("message", receive);
  opener.postMessage(payload, location.origin);
  return true;
}

/** The no-opener callback can show a one-use code; redemption still checks its bound request/state. */
export function manualApprovalCode(params: URLSearchParams): string | null {
  const state = params.get("state"), code = params.get("code");
  return !params.has("error") && state && /^[A-Za-z0-9_-]{32,512}$/.test(state) && code && /^obc_[^\s\x00-\x1f\x7f]{1,16380}$/.test(code) ? code : null;
}
