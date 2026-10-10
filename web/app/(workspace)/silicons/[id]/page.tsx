import {SiliconDetail} from "@/components/product/account-pages";
export default async function Route({params}:{params:Promise<{id:string}>}){return <SiliconDetail id={decodeURIComponent((await params).id)}/>;}
