"use client";
import {useState, type ReactNode} from "react";
import {useQuery, useQueryClient} from "@tanstack/react-query";
import {Button} from "@/components/arc/button/button";
import {ErrorAlert} from "@/components/foundation/feedback/error-alert";
import {ApiError} from "@/lib/errors";
import {request, type RequestOptions} from "@/lib/client/api";
import styles from "./product.module.css";
export interface Envelope<T>{type:string;data:T}
export async function call<T>(path:string, options:RequestOptions={}):Promise<T>{
 const reply=await request<Envelope<T>|null>(path,options);return reply?.data as T;
}
export function useResource<T>(path:string, initialData?:T){return useQuery({queryKey:[path],queryFn:({signal})=>call<T>(path,{signal}),initialData,refetchInterval:10000});}
export function useAction(){const cache=useQueryClient();const [error,setError]=useState<unknown>(null);const [busy,setBusy]=useState(false);const [success,setSuccess]=useState("");
 return {busy,error,success,reset:()=>{setError(null);setSuccess("");},run:async(fn:()=>Promise<unknown>,message="Saved")=>{setError(null);setSuccess("");setBusy(true);try{await fn();await cache.invalidateQueries();setSuccess(message);return true;}catch(e){setError(e);return false;}finally{setBusy(false);}}};}
export function Feedback({action}:{action:ReturnType<typeof useAction>}){return <>{action.error?<ErrorAlert error={ApiError.from(action.error)}/>:null}{action.success?<p role="status" className={styles.notice}>{action.success}</p>:null}</>;}
export function ResourceError({error}:{error:unknown}){return error?<ErrorAlert error={ApiError.from(error)}/>:null;}
export function Empty({children}:{children:ReactNode}){return <p className={styles.empty}>{children}</p>;}
export function More({cursor,onMore}:{cursor?:string|null;onMore:()=>void}){return cursor?<Button variant="secondary" onClick={onMore}>Load more</Button>:null;}
export function safeUrl(url:string){try{const u=new URL(url);return ["http:","https:"].includes(u.protocol)?u.href:undefined;}catch{return undefined;}}
