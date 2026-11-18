"use client";

import { useEffect, useId, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Input } from "@/components/silicon-ui/input/input";
import { Avatar } from "@/components/silicon-ui/avatar/avatar";
import { useSessionView } from "@/lib/client/session";
import { call, ResourceError } from "./common";
import styles from "./people.module.css";

export interface Person {
  uuid: string;
  id: string;
  type: "carbon" | "silicon";
  display_name: string;
  pfp_url?: string | null;
  contact_photo_url?: string | null;
  trust: number;
  blocked: boolean;
}

export function PersonCard({ person }: {person: Person}) {
  const [failed, setFailed] = useState<string>();
  const photo = person.contact_photo_url?.startsWith("/api/v2/contacts/photos/") ? "/api" + person.contact_photo_url : undefined;
  const name = person.display_name || person.id;
  return <span className={styles.person}>
    {photo && failed !== photo ? /* eslint-disable-next-line @next/next/no-img-element -- Private PNG requires the session cookie. */
      <img className={styles.photo} data-sq="surface" src={photo} alt="" onError={() => setFailed(photo)} /> :
      <Avatar name={name} src={person.pfp_url || undefined} size="sm" />}
    <span className={styles.identity}><strong>{name}</strong>{name !== person.id && <small>{person.id}</small>}</span>
  </span>;
}

export function PersonLabel({ id, name }: {id:string; name?:string}) {
  const {account} = useSessionView();
  const profile = useQuery({
    queryKey: ["person", account?.uuid, id],
    queryFn: ({signal}) => call<Person>("/api/v2/people/resolve?id=" + encodeURIComponent(id), {signal}),
    enabled: Boolean(account && /^(c:|si:|[a-f0-9]{8}-)/i.test(id)),
    staleTime: 60_000, retry: false,
  });
  return profile.data ? <PersonCard person={profile.data}/> : <span>{name || id}</span>;
}

export function PersonPicker({ label, kind, value, onChange }: {
  label:string; kind?:"carbon"|"silicon"; value:Person|null; onChange:(person:Person|null)=>void;
}) {
  const {account} = useSessionView();
  const [text,setText]=useState("");
  const [term,setTerm]=useState("");
  const [open,setOpen]=useState(false);
  const [index,setIndex]=useState(-1);
  const listId=useId();
  useEffect(()=>{const timer=setTimeout(()=>setTerm(text.trim()),220);return ()=>clearTimeout(timer);},[text]);
  const results=useQuery({
    queryKey:["people",account?.uuid,kind,term],
    queryFn:({signal})=>call<{items:Person[]}>("/api/v2/people?" + new URLSearchParams({q:term,...(kind?{kind}:{})}),{signal}),
    enabled:open && !value, staleTime:15_000, retry:false,
  });
  const items=text.trim()===term ? results.data?.items : undefined;
  const exact=items?.find(p=>p.id.toLowerCase()===term.toLowerCase() || p.uuid===term);
  useEffect(()=>{
    if(exact && !value && open){
      const timer=setTimeout(()=>{onChange(exact);setOpen(false);setText("");},0);
      return ()=>clearTimeout(timer);
    }
  },[exact, value, open, onChange]);

  function choose(person:Person){onChange(person);setText("");setIndex(-1);setOpen(false);}
  if(value) return <div className={styles.selection}><span className={styles.label}>{label}</span>
    <div className={styles.selected}><PersonCard person={value}/><button type="button" onClick={()=>{onChange(null);setText("");setOpen(true);}}>Change</button></div></div>;
  return <div className={styles.picker} onBlur={e=>{if(!e.currentTarget.contains(e.relatedTarget))setOpen(false);}}>
    <Input label={label} placeholder={kind==="silicon"?"Name or si:id":"Name, c:id or si:id"} role="combobox" autoComplete="off"
      aria-expanded={open} aria-controls={listId} aria-autocomplete="list" aria-activedescendant={open && index>=0 ? listId+"-"+index : undefined}
      value={text} onFocus={()=>setOpen(true)} onChange={e=>{setText(e.target.value);setOpen(true);setIndex(-1);}}
      onKeyDown={e=>{
        if(e.key==="Escape"){setOpen(false);setIndex(-1);}
        if((e.key==="ArrowDown"||e.key==="ArrowUp") && items?.length){e.preventDefault();setOpen(true);setIndex(i=>e.key==="ArrowDown"?(i+1)%items.length:(i-1+items.length)%items.length);}
        if(e.key==="Enter"){e.preventDefault();const candidate=items?.[index]??exact;if(candidate)choose(candidate);}
      }}/>
    {open && <div className={styles.options} data-sq="surface"><div role="listbox" id={listId} aria-label={label}>
      {items?.map((person,i)=><button type="button" role="option" id={listId+"-"+i} aria-selected={i===index} key={person.uuid}
        onMouseDown={e=>e.preventDefault()} onClick={()=>choose(person)}>
        <PersonCard person={person}/>{person.trust>0 && <small>Trust {person.trust}</small>}
      </button>)}</div>
      <ResourceError error={results.error}/>
      {!results.error && !items?.length && <p role="status">{results.isFetching || term!==text.trim()?"Searching…":term?"No matching accounts.":"Enter an account ID to find someone."}</p>}
    </div>}
  </div>;
}
