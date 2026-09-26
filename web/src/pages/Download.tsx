import { Show } from "solid-js";
import { DOWNLOADS, type Platform } from "../config";
import { Link } from "../lib/router";

/** Placeholder until the Extend apps are published; links are configured in src/config.ts. */
export default function Download(props: { platform: string }) {
  const download = () => DOWNLOADS[props.platform as Platform];
  return (
    <section class="page-main narrow" data-testid="download-page">
      <Show
        when={download()}
        fallback={
          <>
            <p class="eyebrow">Download</p>
            <h1 class="page-title">No such download.</h1>
            <p class="lead">
              There is no Extend app called <code>{props.platform}</code>. See <Link href="/devices/new">Add a device</Link> for the right one.
            </p>
          </>
        }
      >
        {(d) => (
          <>
            <p class="eyebrow">Download</p>
            <h1 class="page-title">{d().app}.</h1>
            <p class="lead">{d().note}</p>
            <div class="notice">
              <p>
                This download isn't published yet. When it is, this page gives you the file. Until then, follow the Extend repository for releases:{" "}
                <a href="https://github.com/teamofsilicons/silicon-extend" target="_blank" rel="noopener noreferrer">
                  github.com/teamofsilicons/silicon-extend
                </a>
                .
              </p>
            </div>
            <p>
              <Link href="/devices/new">Back to adding a device</Link>
            </p>
          </>
        )}
      </Show>
    </section>
  );
}
