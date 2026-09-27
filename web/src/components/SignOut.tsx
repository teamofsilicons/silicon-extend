import { createSignal, Show } from "solid-js";
import { LogOut } from "lucide-solid";
import { session } from "../lib/session";
import { navigate } from "../lib/router";
import { Button, Modal } from "./ui";

/**
 * What signing out does, in the Carbon's words. Carbon decision 1: a Carbon signing out of Extend
 * (website or CLI) ends the running sessions of the Silicons that Carbon gave access to, and never
 * another Carbon's; a Silicon signing out ends its own sessions.
 */
export function signOutEffect(type: "carbon" | "silicon" | undefined): string {
  return type === "silicon"
    ? "Signing out ends your running sessions."
    : "Signing out ends the running sessions of the Silicons you gave access to, on every device you paired. Silicons other Carbons gave access to carry on. Your Silicons keep their access.";
}

/** Sign out, after saying what it ends. `icon` is the top bar's button; otherwise a plain button. */
export function SignOutButton(props: { icon?: boolean }) {
  const s = session();
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  async function signOut() {
    setBusy(true);
    // Go to "/" first so the sign-in form appears once, not twice.
    navigate("/", { replace: true });
    await s.signOut().catch(() => undefined);
    setBusy(false);
    setOpen(false);
  }
  return (
    <>
      <Show
        when={props.icon}
        fallback={
          <Button onClick={() => setOpen(true)} data-testid="settings-sign-out">
            Sign out
          </Button>
        }
      >
        <button class="icon-button" aria-label="Sign out" title="Sign out" data-testid="sign-out" onClick={() => setOpen(true)}>
          <LogOut size={16} />
        </button>
      </Show>
      {/* Mounted only while open: the top bar and Settings each have a Sign out. */}
      <Show when={open()}>
        <Modal open title="Sign out of Extend?" onClose={() => setOpen(false)} testid="sign-out-dialog">
          <p data-testid="sign-out-effect">{signOutEffect(s.member()?.type)}</p>
          <div class="modal-actions">
            <Button variant="ghost" onClick={() => setOpen(false)}>
              Stay signed in
            </Button>
            <Button variant="primary" busy={busy()} onClick={signOut} data-testid="sign-out-confirm">
              Sign out
            </Button>
          </div>
        </Modal>
      </Show>
    </>
  );
}
