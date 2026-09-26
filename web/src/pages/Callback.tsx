import { createSignal, onMount, Show } from "solid-js";
import { session } from "../lib/session";
import { toApiError, type ApiError } from "../lib/api";
import { finishIamLogin } from "../lib/auth";
import { navigate } from "../lib/router";
import { read, remove } from "../lib/storage";
import { ErrorNote, Spinner } from "../components/ui";
import { Link } from "../lib/router";

/** Where Silicon IAM sends the browser back with `?slt=…&state=…`. */
export default function Callback() {
  const s = session();
  const [error, setError] = createSignal<ApiError | null>(null);

  onMount(async () => {
    const params = new URLSearchParams(location.search);
    // The SLT must not linger in the address bar or history.
    history.replaceState(null, "", "/auth/callback");
    const w = s.world();
    try {
      const slt = finishIamLogin(params, w.kind === "production" ? "production" : w.environment.environment_id);
      await s.client().login(slt);
      s.clearSignedOutReason();
      const next = read("session", "bridge.next") || "/devices";
      remove("session", "bridge.next");
      navigate(next.startsWith("/") && !next.startsWith("//") ? next : "/devices", { replace: true });
    } catch (e) {
      setError(toApiError(e));
    }
  });

  return (
    <section class="page narrow">
      <Show when={error()} fallback={<Spinner label="Signing you in…" />}>
        <h1 class="page-title">Sign-in didn't finish</h1>
        <ErrorNote error={error()} />
        <p>
          <Link href="/sign-in" class="button primary">
            Back to sign-in
          </Link>
        </p>
      </Show>
    </section>
  );
}
