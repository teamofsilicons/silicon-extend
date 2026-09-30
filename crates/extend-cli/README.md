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
