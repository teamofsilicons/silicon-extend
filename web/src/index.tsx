import { deliverApproval } from "./lib/approval-popup";
import { createSignal, onMount } from "solid-js";
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
  return <main class="page-main narrow"><h1>{sent() ? "Finishing approval…" : "Return to Extend"}</h1><p>{sent() ? "Your Extend tab is saving the approved access. This window closes when it is ready." : "Start a new approval from Extend Settings."}</p></main>;
}
render(() => approvalParams ? <ApprovalCallback /> : <App />, document.getElementById("root")!);
