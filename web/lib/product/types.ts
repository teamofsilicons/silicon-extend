export interface Account{uuid:string;id:string;type:"carbon"|"silicon";display_name?:string}
export interface Device{device_id:string;name:string;os:string;kind:string;owner:Account;state:string;online:boolean;awake?:boolean;sleep_state?:string;last_seen_at?:string;in_use?:{silicon_id:string;session_id:string;since:string};in_use_by_other?:boolean;in_use_by_other_carried?:boolean;pair_ttl_days?:number;days_left?:number;version:number;removed_at?:string;in_use_indicator?:"shown"|"hidden";wake_muted?:boolean;capabilities?:string[];missing?:{capability:string;reason:string}[];app_version?:string;engine_version?:string;host_device_id?:string}
export interface Page<T>{items:T[];next_cursor?:string}
export interface Grant{device_id:string;device_name?:string;silicon_id:string;silicon_uuid?:string;granted_by:string;granted_at:string;wake_muted?:boolean}
export interface Session{session_id:string;device_id:string;silicon_id:string;state:string;started_at:string;ended_at?:string;end_reason?:string;command_count?:number}
export interface Activity{id:string;at:string;actor:Account;action:string;command?:string;outcome?:string;files?:string[]}
export interface FileInfo{file_id:string;name:string;kind:string;content_type:string;size_bytes:number;url:string;permanent:boolean;self_destruct_at?:string}
export interface Setup{state:string;steps:{key:string;title:string;status:string;help?:string;error?:string;input?:string}[]}
export interface Silicon extends Account{account?:Account;looked_after?:boolean;granted_by_you?:number;running_sessions?:number;grants?:number}
export interface Wake{wake_id:string;device_id:string;from:string;reason:string;state:string;expires_at:string;device_notice:string;device_notice_note?:string}
export interface Request{request_id:string;device_id:string;from:string;to:string;reason:string;created_at:string;delivery:string;routed_to?:string}
