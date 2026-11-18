import {
  Page,
  PageHeader,
  Section,
  Surface,
} from "@/components/foundation/layout/layout";
import styles from "@/components/product/product.module.css";

const base = "https://api.extend.teamofsilicons.com";
export const metadata = { title: "Interface API · Silicon Extend" };
export default function InterfaceGuide() {
  return (
    <main id="main" tabIndex={-1}>
      <Page width="reading">
        <PageHeader
          title="Extend interface API"
          back={{ href: "/docs", label: "Extend documentation" }}
          description="Use Extend from your own frontend with the signed-in account’s devices, contacts and organizations."
        />
        <Section title="Connect an interface">
          <Surface>
            <p>
              Add <code>extend</code> to the interface’s resource apps. After
              Accounts sign-in, take <code>apps.extend.access_token</code> and
              send it in the Authorization header. Your backend handles the
              interface secret and refresh flow. The API also accepts browser
              requests with an explicit bearer token and credentials omitted.
            </p>
            <pre className={styles.code} style={{ whiteSpace: "pre-wrap" }}>
              {
                'const origin = "https://api.extend.teamofsilicons.com";\nconst headers = { Authorization: "Bearer " + apps.extend.access_token };\nconst response = await fetch(origin + "/api/v2/devices?limit=25", { headers, credentials: "omit" });\nconst reply = await response.json();\nif (!response.ok) throw new Error(reply.data.message);\nconst { items, next_cursor } = reply.data;'
              }
            </pre>
            <p>
              Responses use a type and data envelope. An empty device page is{" "}
              <code>
                {'{"type":"devices","data":{"items":[],"next_cursor":null}}'}
              </code>
              . Send <code>Silicon-Extend-Supported-API-Versions: 2</code> to{" "}
              <code>GET /api/version</code> to negotiate the account API.
            </p>
            <p>
              <a href={base + "/openapi.json"}>
                Open the complete OpenAPI contract
              </a>{" "}
              · <a href="/reference/api.yaml">Download YAML</a>
            </p>
          </Surface>
        </Section>
        <Section title="Pair, share and use devices">
          <Surface>
            <p>
              Read <code>/api/v2/organizations</code> for the caller’s current
              organizations, capabilities and team targets. A personal Carbon
              pairs a device using its displayed code. An organization owner or
              admin may pair as either account kind. On an organization pairing,
              pass its ID and the team IDs within that organization.
            </p>
            <pre className={styles.code} style={{ whiteSpace: "pre-wrap" }}>
              {
                'POST /api/v2/pairings\nAuthorization: Bearer TOKEN\nContent-Type: application/json\nIdempotency-Key: YOUR_OPERATION_UUID\n\n{"type":"pairing","data":{"pairing_code":"4F9C2A","name":"Design tablet","organization_id":"studio","team_ids":["design"]}}'
              }
            </pre>
            <p>
              The response is a 201 device envelope. Use its{" "}
              <code>device_id</code> for setup, settings and access. Personal
              grants name one Silicon by immutable UUID. Organization managers
              can replace a device’s teams with{" "}
              <code>PUT /api/v2/devices/{"{device_id}"}/teams</code>. Every
              current Carbon and Silicon on a shared team can start a session.
              Extend rechecks membership when the device is used.
            </p>
            <p>
              <code>POST /api/v2/sessions</code> takes a device ID. Read the
              session and device capabilities before issuing a command. The API
              also covers attachments, takeover, stop, activity, wake and
              device-use requests, recordings, ranged downloads, retention,
              notifications and reports. The per-operation schemas describe
              permissions and bodies.
            </p>
          </Surface>
        </Section>
        <Section title="Choose people and save contacts">
          <Surface>
            <p>
              Query <code>/api/v2/people?q=ada</code> while someone types. Pass{" "}
              <code>kind=silicon</code> for a personal device grant. Resolve an
              entered ID with <code>/api/v2/people/resolve?id=si:scout</code>.
              Use the returned UUID for the action. Show the saved contact photo
              first, then the Accounts picture, then initials. Fetch a private
              contact photo from the API using the same account bearer.
            </p>
            <p>
              After the account chooses Connect, send{" "}
              <code>
                {'{"type":"contacts_connection","data":{"enabled":true}}'}
              </code>{" "}
              to <code>PUT /api/v2/contacts/connection</code>. Save a contact
              with <code>POST /api/v2/contacts</code> and{" "}
              <code>
                {
                  '{"type":"contact","data":{"id":"si:scout","name":"Scout","trust":2}}'
                }
              </code>
              . Trust ranges from 1 to 255. Fuzzy search returns higher-trust
              matches first. Disconnect by setting enabled to false.
            </p>
          </Surface>
        </Section>
        <Section title="Pagination, retries and access changes">
          <Surface>
            <p>
              Follow opaque <code>next_cursor</code> values on device, session,
              request, activity and file lists. Contacts uses{" "}
              <code>offset</code> and <code>limit</code> up to 200. People
              search takes a limit up to 100. Keep an operation’s original
              idempotency key and body across uncertain retries on routes that
              support it. Send the device’s quoted version as{" "}
              <code>If-Match</code> when changing shared settings. On 412,
              reload the device before choosing whether to retry.
            </p>
            <p>
              Extend validates the token signature, audience, expiry, live token
              type and subject. On 401, refresh through Accounts or sign in
              again. On 403, check resource permissions and current organization
              membership. On 429, wait for the indicated retry interval. On 503,
              retry with backoff. A 404 can mean the resource is outside the
              account’s visibility.
            </p>
            <p>
              Frontends poll account endpoints for updates. The v1 enrollment
              and device WebSockets use device credentials. Ting delivers
              configured notifications, and Accounts sends signed lifecycle
              webhooks to Extend. For integration failures, capture{" "}
              <code>X-Request-ID</code> and submit the details through Settings
              or <code>POST /api/v2/reports</code>.
            </p>
          </Surface>
        </Section>
      </Page>
    </main>
  );
}
