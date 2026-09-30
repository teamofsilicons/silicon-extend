import { For, Show } from "solid-js";
import { DOWNLOADS, releaseAsset, type Platform } from "../config";
import { Link } from "../lib/router";

/** The files and release details for one platform; configured in src/config.ts. */
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
            <p class="eyebrow" data-testid="download-version">
              Download · version {d().release.version}
            </p>
            <h1 class="page-title">{d().app}.</h1>
            <p class="lead">{d().note}</p>
            <Show
              when={d().files}
              fallback={
                <div class="notice">
                  <p>
                    This download isn't published yet. When it is, this page gives you the file. Until then, follow the Extend repository for releases:{" "}
                    <a href="https://github.com/teamofsilicons/silicon-extend" target="_blank" rel="noopener noreferrer">
                      github.com/teamofsilicons/silicon-extend
                    </a>
                    .
                  </p>
                </div>
              }
            >
              {(files) => (
                <>
                  <div class="download-files" data-testid="download-files">
                    <For each={files()}>
                      {(f) => (
                        <a class="button primary" href={releaseAsset(d().release, f.name)} data-testid="download-file">
                          {f.label}
                        </a>
                      )}
                    </For>
                  </div>
                  <p class="fine">
                    See the <a href={d().release.url} target="_blank" rel="noopener noreferrer">release notes</a>. Check a file against its{" "}
                    <a href={releaseAsset(d().release, "SHA256SUMS")}>SHA-256 checksum</a> with <code>shasum -a 256</code>.
                  </p>
                </>
              )}
            </Show>
            <p>
              <Link href="/devices/new">Back to adding a device</Link>
            </p>
          </>
        )}
      </Show>
    </section>
  );
}
