import { deliverApproval, manualApprovalCode } from "./lib/approval-popup";
import { createSignal, onMount, Show } from "solid-js";
import { render } from "solid-js/web";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "@fontsource/source-serif-4/latin-400.css";
import "@fontsource/source-serif-4/latin-400-italic.css";
import "./styles.css";
import "./uiarc.css";
import App from "./App";

const approvalParams = location.pathname === "/auth/obo/callback" ? new URLSearchParams(location.search) : null;
if (approvalParams) history.replaceState(null, "", "/auth/obo/callback");
function ApprovalCallback() {
  const [sent, setSent] = createSignal(false);
  onMount(() => setSent(deliverApproval(approvalParams!)));
  const code = manualApprovalCode(approvalParams!);
  return <main class="page-main narrow"><h1>{sent() ? "Finishing approval…" : "Return to Extend"}</h1><p>{sent() ? "Your Extend tab is saving the approved access. This window closes when it is ready." : approvalParams!.has("error") ? "Access was not approved. Return to your Extend tab; your sign-in and pending action are unchanged." : code ? "Copy this single-use approval code into your original Extend tab to finish the same request." : "Return to Extend Settings to review access."}</p><Show when={!sent() && code}><label for="approval-code">Single-use approval code</label><textarea id="approval-code" readOnly value={code!} autocomplete="off" spellcheck={false} /></Show></main>;
}
render(() => approvalParams ? <ApprovalCallback /> : <App />, document.getElementById("root")!);
