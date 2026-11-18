"use client";
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Section, Surface } from "@/components/foundation/layout/layout";
import { useSessionView } from "@/lib/client/session";
import type { Device, Session } from "@/lib/product/types";
import { call, Feedback, ResourceError, useAction } from "./common";
import styles from "./product.module.css";
import peopleStyles from "./people.module.css";

export interface OrganizationContext {
  organizations: {
    org_id: string;
    name: string;
    capabilities: { admin: boolean; owner: boolean };
  }[];
  share_targets: { org_id: string; type: string; id: string; name: string }[];
}
export function useOrganizations() {
  const { account } = useSessionView();
  return useQuery({
    queryKey: ["organizations", account?.uuid],
    queryFn: ({ signal }) =>
      call<OrganizationContext>("/api/v2/organizations", { signal }),
    enabled: !!account,
    staleTime: 30_000,
    retry: false,
  });
}
export function Teams({
  context,
  org,
  selected,
  onChange,
}: {
  context: OrganizationContext;
  org: string;
  selected: string[];
  onChange: (ids: string[]) => void;
}) {
  const teams = context.share_targets.filter(
    (t) => t.org_id === org && t.type === "team",
  );
  return (
    <fieldset className={peopleStyles.teams}>
      <legend>Teams with device access</legend>
      {teams.map((team) => {
        const id = team.id.slice(org.length + 1);
        return (
          <label key={id} className={styles.check}>
            <input
              type="checkbox"
              checked={selected.includes(id)}
              onChange={(e) =>
                onChange(
                  e.target.checked
                    ? [...selected, id]
                    : selected.filter((t) => t !== id),
                )
              }
            />
            {team.name}
          </label>
        );
      })}
      {!teams.length && (
        <p className={styles.muted}>This organization has no teams.</p>
      )}
    </fieldset>
  );
}
export function OrganizationSharing({ device }: { device: Device }) {
  const context = useOrganizations();
  const org = device.organization!;
  const [selected, setSelected] = useState<string[] | null>(null);
  const [editVersion, setEditVersion] = useState<number>();
  const action = useAction();
  if (device.host_device_id)
    return (
      <Section title="Team access">
        <Surface>
          Team access is inherited from{" "}
          <a href={"/devices/" + device.host_device_id}>the host device</a>.
        </Surface>
      </Section>
    );
  return (
    <Section title="Team access">
      <Surface>
        <p>
          Every current Carbon and Silicon in a selected team can use this
          device.
        </p>
        <ResourceError error={context.error} />
        {context.data && (
          <form
            className={styles.form}
            onSubmit={(e) => {
              e.preventDefault();
              void action.run(async () => {
                await call("/api/v2/devices/" + device.device_id + "/teams", {
                  method: "PUT",
                  headers: {
                    "If-Match": '"' + (editVersion ?? device.version) + '"',
                  },
                  body: {
                    type: "team_access",
                    data: { team_ids: selected ?? org.team_ids },
                  },
                });
                setSelected(null);
                setEditVersion(undefined);
              }, "Team access saved");
            }}
          >
            <Teams
              context={context.data}
              org={org.id}
              selected={selected ?? org.team_ids}
              onChange={(ids) => {
                if (selected === null) setEditVersion(device.version);
                setSelected(ids);
              }}
            />
            <div className={styles.actions}>
              <Button
                type="submit"
                disabled={selected === null}
                loading={action.busy}
              >
                Save team access
              </Button>
              {selected && (
                <Button
                  variant="ghost"
                  onClick={() => {
                    setSelected(null);
                    setEditVersion(undefined);
                  }}
                >
                  Cancel changes
                </Button>
              )}
            </div>
          </form>
        )}
        <Feedback action={action} />
      </Surface>
    </Section>
  );
}
export function UseDevice({ device }: { device: Device }) {
  const action = useAction();
  const { account } = useSessionView();
  const mine =
    device.in_use?.silicon_id === account?.id
      ? device.in_use?.session_id
      : undefined;
  return (
    <Section title="Use this device">
      <Surface>
        <Feedback action={action} />
        <div className={styles.actions}>
          {mine ? (
            <>
              <Button
                variant="secondary"
                loading={action.busy}
                onClick={() =>
                  void action.run(
                    () =>
                      call("/api/v2/sessions/" + mine + "/commands", {
                        method: "POST",
                        body: {
                          type: "command",
                          data: { command: "screenshot", args: [] },
                        },
                      }),
                    "Screenshot saved to Recordings",
                  )
                }
              >
                Take screenshot
              </Button>
              <Button
                variant="secondary"
                loading={action.busy}
                onClick={() =>
                  void action.run(
                    () =>
                      call("/api/v2/sessions/" + mine + "/end", {
                        method: "POST",
                      }),
                    "Session ended",
                  )
                }
              >
                End my session
              </Button>
            </>
          ) : (
            <Button
              disabled={
                !device.online ||
                device.state !== "ready" ||
                !!device.in_use ||
                !!device.in_use_by_other
              }
              loading={action.busy}
              onClick={() =>
                void action.run(
                  () =>
                    call<Session>("/api/v2/sessions", {
                      method: "POST",
                      idempotencyKey: true,
                      body: {
                        type: "session",
                        data: { device_id: device.device_id },
                      },
                    }),
                  "Session started",
                )
              }
            >
              Start session
            </Button>
          )}
        </div>
      </Surface>
    </Section>
  );
}
