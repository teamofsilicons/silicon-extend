import Link from "next/link";
import {notFound} from "next/navigation";
import {readFile} from "node:fs/promises";
import {join} from "node:path";
import {Page,PageHeader,Section,Surface} from "@/components/foundation/layout/layout";
import styles from "@/components/product/product.module.css";
export default async function Docs({params}:{params:Promise<{slug?:string[]}>}){
 const {slug}=await params;const page=slug?.join("/")||"start";
 if(!["start","devices","cli","how-it-works"].includes(page))notFound();
 const cli=page==="cli"?await readFile(join(process.cwd(),"public/reference/cli.yaml"),"utf8"):"";
 return <main id="main" tabIndex={-1}><Page width="reading"><PageHeader title={page==="start"?"Extend documentation":page==="devices"?"Pairing each kind of device":page==="cli"?"CLI reference":"How Extend works"} back={{href:"/",label:"Silicon Extend"}}/><nav className={styles.row} aria-label="Documentation">{[["start","Start here"],["devices","Devices"],["cli","CLI reference"],["how-it-works","How it works"]].map(([key,label])=><Link aria-current={key===page?"page":undefined} key={key} href={key==="start"?"/docs":`/docs/${key}`}>{label}</Link>)}</nav>
 {page==="start"?<><Section title="For Carbons"><Surface><p>Sign in with Silicon Accounts, install a device app, and enter its pairing code in Add a device. Give exact Silicons access by their si: id.</p><p>Open the device to follow its sessions, recordings and activity, change its pairing lifetime, or stop access. The Silicons you look after also appear in Your Silicons.</p><Link href="/devices/new">Pair your first device →</Link></Surface></Section><Section title="For Silicons"><Surface><pre className={styles.code}>{`silicon-apps install extend
extend login --slt-stdin
extend device ls
extend session new <device> --connect
extend snapshot -i
extend session end <session> --yes`}</pre><p>Obtain a short-lived token for Extend from Silicon Accounts and pass it on standard input. Run extend --help for every available command.</p></Surface></Section></>:null}
 {page==="devices"?<Section title="Choose a connection"><Surface><p><strong>macOS, Windows and Linux:</strong> install the matching desktop app, open it, then enter its pairing code. Follow the permissions requested on the device.</p><p><strong>Android and Android TV:</strong> install the Android app and pair its code. Grant the accessibility and screen permissions requested by the app.</p><p><strong>iPhone and iPad:</strong> pair a computer first. In Add a device, choose Through my computer, select it and the device type. Keep the device connected and unlocked during setup.</p><p><strong>Apple TV, Samsung TV and LG TV:</strong> connect through a paired computer on the same network. Enter an address if discovery cannot find the TV. Follow the device’s code and setup steps.</p><p>Setup explains failed permissions and offers a retry. The device’s capability list shows what it can do.</p><Link href="/downloads">Download device apps →</Link></Surface></Section>:null}
 {page==="how-it-works"?<Section title="Access follows the pairing"><Surface><p>Each pairing belongs to its Carbon. Granting a Silicon access lets it start sessions through that pairing. A device may have other pairings; their private activity stays separate.</p><p>A custodian can inspect their Silicon’s grants, sessions, files and requests and can end sessions or renounce a grant. They cannot run device commands as that Silicon.</p><p>Revoking access ends the affected sessions. Signing out of Extend ends sessions on the pairings you granted. Removing a pairing ends its access. The device’s in-use banner can be changed from its settings.</p><p>Requests remain visible in Extend when Ting delivery is off. Wake requests can be muted per device or per Silicon. Recordings and captures keep their expiry until you choose Keep file.</p></Surface></Section>:null}
 {page==="cli"?<Section title="Commands and flags"><Surface><p>The full CLI contract lists command grammar, arguments, outputs and errors. Use your browser’s Find to locate a command.</p><p><a href="/reference/cli.yaml" download>Download CLI reference</a> · <a href="/reference/api.yaml" download>Download HTTP API reference</a></p><pre className={styles.code} style={{whiteSpace:"pre-wrap"}}>{cli}</pre></Surface></Section>:null}
 </Page></main>;
}
