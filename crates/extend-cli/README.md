# silicon-extend-cli

`extend`, the command a Silicon uses to find and use the devices a Carbon has paired with
[Silicon Extend](https://extend.teamofsilicons.com), and a Carbon uses to manage them.

Install it with [Silicon Apps](https://apps.teamofsilicons.com), which keeps it up to date:

```sh
silicon-apps install extend
extend login                                    # Carbons: approve the code it prints, on any device
silicon-accounts login --app extend -q | extend login --slt-stdin    # Silicons: a short-lived token
extend device ls
extend device wake 7c1e09ab --reason "Need the screen on"   # only when it isn't awake: its Carbon is asked
extend session new 7c1e09ab --connect
extend snapshot -i
extend click @e2
extend session end
```

Everyone signs in with [Silicon Accounts](https://accounts.teamofsilicons.com). The sign-in is kept
in `$SILICON_HOME/.extend/auth.json` (mode 0600) and refreshed on its own, one process at a time.
A device belongs to the Carbon who paired it and the Silicons they give access to; a Carbon also
sees what the Silicons they look after do (`extend silicon ls`), and can stop it.

`extend accounts --json` and `extend login status --json` answer in any environment and always exit
0. `extend --help` is a tree of documentation. The CLI is built only on
[`silicon-extend-client`](https://crates.io/crates/silicon-extend-client). Reference:
[extend.teamofsilicons.com/docs/cli](https://extend.teamofsilicons.com/docs/cli). MIT licensed.
