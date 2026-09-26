import { createSignal } from "solid-js";
import { ApiError, toApiError } from "../lib/api";
import { APP_SECRET, session } from "../lib/session";
import { navigate } from "../lib/router";
import { Button, ErrorNote, toast } from "./ui";

/**
 * Enter a test application's app_secret. Extend validates it (GET /api/v1/testing-environment)
 * before anything switches, so a bad secret never leaves the tab half in a test world.
 */
export function TestingSecretForm() {
  const s = session();
  const [secret, setSecret] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<ApiError | null>(null);

  async function submit(event: Event) {
    event.preventDefault();
    const value = secret().trim();
    setError(null);
    if (!APP_SECRET.test(value)) {
      setError(
        new ApiError(0, {
          code: "invalid_input",
          message: "That isn't a test app secret. It starts with ask_ followed by 43 letters, digits, - or _.",
          hint: "Copy the test application's app_secret from Honeycomb. It is not the Honeycomb testing_key.",
        }),
      );
      return;
    }
    setBusy(true);
    try {
      const environment = await s.client().testingEnvironment(value);
      s.enterTesting(value, environment);
      setSecret("");
      toast(`Entered test environment ${environment.name}`, "info");
      navigate("/", { replace: true });
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form class="testing-form" onSubmit={submit} data-testid="testing-form">
      <label for="app-secret">Test application secret</label>
      <div class="input-row">
        <input
          id="app-secret"
          type="password"
          autocomplete="off"
          spellcheck={false}
          placeholder="ask_…"
          value={secret()}
          data-testid="testing-secret-input"
          onInput={(e) => setSecret(e.currentTarget.value)}
        />
        <Button type="submit" busy={busy()} data-testid="testing-secret-submit">
          Enter
        </Button>
      </div>
      <p class="fine">
        Selects that application's test environment, managed in Honeycomb. It has its own devices, access and sessions, and its own sign-in. Nothing you do there touches production. The secret stays in this tab only.
      </p>
      <ErrorNote error={error()} />
    </form>
  );
}
