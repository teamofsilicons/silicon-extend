# silicon-extend-cli

`extend`, the command a Silicon uses to find and use the devices a Carbon has paired with
[Silicon Extend](https://extend.teamofsilicons.com), and a Carbon uses to manage them.

Install it with Honeycomb (`honeycomb install extend`) or from crates.io:

```sh
cargo install silicon-extend-cli
extend login <slt>                      # a short-lived token from Silicon IAM
extend device ls
extend device wake 7c1e09ab --reason "Need the screen on"   # only when it isn't awake: its Carbon is asked
extend session new 7c1e09ab --connect
extend snapshot -i
extend click @e2
extend session end
```

`extend --help` is a tree of documentation. The CLI is built only on
[`silicon-extend-client`](https://crates.io/crates/silicon-extend-client). Reference:
[extend.teamofsilicons.com/docs/cli](https://extend.teamofsilicons.com/docs/cli). MIT licensed.

Each IAM 5 login selects one account and organization. Sign in once for each context, then use
`extend login contexts` and `extend login use <account> <organization>` to switch. `--team <org>`
restores the current account's saved credentials for one command. Tokens and device-session caches
are isolated by API origin, account, organization, and production/test environment. Older unscoped
logins need a fresh IAM sign-in.

Devices are private when paired, attached, or imported. `extend device importable` lists your
configured devices from other organizations; `extend device import <id>` adds one to the selected
organization. Use `--visibility team` or `extend device visibility <id> team` to make it discoverable
there. `extend device ls --team-visible` lists shared devices; Silicon control still requires an
explicit owner grant. `personal` hides the device from every other member, including a previously
granted Silicon. Removing a device removes this organization's binding; its physical setup remains
available to import again. Pass `--key <uuid>` to retry an import with the same operation key.
