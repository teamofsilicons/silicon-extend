"use client";
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Page,
  PageHeader,
  Section,
  Surface,
} from "@/components/foundation/layout/layout";
import { Input } from "@/components/silicon-ui/input/input";
import { Button } from "@/components/silicon-ui/button/button";
import { useSessionView } from "@/lib/client/session";
import { call, Feedback, ResourceError, useAction } from "./common";
import { PersonCard, PersonPicker, type Person } from "./people";
import styles from "./product.module.css";

export function Contacts() {
  const { account } = useSessionView();
  const cache = useQueryClient();
  const action = useAction();
  const [person, setPerson] = useState<Person | null>(null);
  const [name, setName] = useState("");
  const [trust, setTrust] = useState("1");
  const [offset, setOffset] = useState(0);
  const connection = useQuery({
    queryKey: ["contacts-connection", account?.uuid],
    queryFn: ({ signal }) =>
      call<{ connected: boolean }>("/api/v2/contacts/connection", { signal }),
  });
  const connected = connection.data?.connected;
  const contacts = useQuery({
    queryKey: ["contacts", account?.uuid, offset],
    queryFn: ({ signal }) =>
      call<{ items: Person[]; total: number }>(
        "/api/v2/contacts?limit=50&offset=" + offset,
        { signal },
      ),
    enabled: connected === true,
  });
  function select(p: Person | null) {
    setPerson(p);
    setName(p?.display_name || p?.id || "");
    setTrust(String(p?.trust || 1));
  }
  async function toggle(enabled: boolean) {
    await call("/api/v2/contacts/connection", {
      method: "PUT",
      body: { type: "contacts_connection", data: { enabled } },
    });
    cache.setQueryData(["contacts-connection", account?.uuid], {
      connected: enabled,
    });
    for (const key of ["person", "people", "contacts"])
      await cache.resetQueries({ queryKey: [key, account?.uuid] });
    select(null);
    setOffset(0);
  }
  return (
    <Page width="narrow">
      <PageHeader
        title="Contacts"
        description="Use your Silicon Contacts address book in Extend."
      />
      <ResourceError error={connection.error} />
      <Feedback action={action} />
      {connected === false && (
        <Surface>
          <p>
            Connect your address book to see saved names and photos, find
            trusted contacts, and save people here.
          </p>
          <Button
            loading={action.busy}
            onClick={() =>
              void action.run(() => toggle(true), "Contacts connected")
            }
          >
            Connect Silicon Contacts
          </Button>
        </Surface>
      )}
      {connected === true && (
        <>
          <Section title={person?.trust ? "Edit contact" : "Add a contact"}>
            <Surface>
              <form
                className={styles.form}
                onSubmit={(e) => {
                  e.preventDefault();
                  if (person)
                    void action.run(async () => {
                      await call("/api/v2/contacts", {
                        method: "POST",
                        body: {
                          type: "contact",
                          data: { id: person.uuid, name, trust: Number(trust) },
                        },
                      });
                      select(null);
                    }, "Contact saved");
                }}
              >
                <PersonPicker
                  label="Carbon or Silicon"
                  value={person}
                  onChange={select}
                />
                {person && (
                  <>
                    <Input
                      required
                      label="Saved name"
                      maxLength={200}
                      value={name}
                      onChange={(e) => setName(e.target.value)}
                    />
                    <Input
                      required
                      label="Trust"
                      type="number"
                      min={1}
                      max={255}
                      description="1 is saved. 2 is trusted. Higher values appear first."
                      value={trust}
                      onChange={(e) => setTrust(e.target.value)}
                    />
                  </>
                )}
                <Button type="submit" disabled={!person} loading={action.busy}>
                  Save contact
                </Button>
              </form>
            </Surface>
          </Section>
          <Section title="Saved contacts">
            <Surface>
              <ResourceError error={contacts.error} />
              {contacts.data?.items.map((p) => (
                <div key={p.uuid} className={styles.item}>
                  <div className={styles.spread}>
                    <PersonCard person={p} />
                    <Button variant="ghost" onClick={() => select(p)}>
                      Edit
                    </Button>
                  </div>
                  <small>
                    Trust {p.trust}
                    {p.blocked ? " · Blocked" : ""}
                  </small>
                </div>
              ))}
              {contacts.data?.total === 0 && <p>Your address book is empty.</p>}
              <div className={styles.actions}>
                {offset > 0 && (
                  <Button
                    variant="secondary"
                    onClick={() => setOffset(offset - 50)}
                  >
                    Previous
                  </Button>
                )}
                {contacts.data && offset + 50 < contacts.data.total && (
                  <Button
                    variant="secondary"
                    onClick={() => setOffset(offset + 50)}
                  >
                    Next
                  </Button>
                )}
              </div>
            </Surface>
          </Section>
          <Button
            variant="ghost"
            loading={action.busy}
            onClick={() =>
              void action.run(() => toggle(false), "Contacts disconnected")
            }
          >
            Disconnect Silicon Contacts
          </Button>
        </>
      )}
    </Page>
  );
}
