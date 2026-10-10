# Silicon Extend CLI 1.1.1

Successful text output from `extend snapshot` now ends with:

> Run --raw to get the entire accessibility tree.

The hint is omitted when `--raw` or `--json` is supplied. The CLI guide now shows
`extend snapshot --raw` and explains that it includes the tree exposed by the device;
it cannot recover controls withheld by the app or Android.

This is a CLI-only patch for all six supported platforms. Device apps, service, client and
protocol remain at 1.1.0. This release does not fix the reported missing Swiggy/Zomato controls
or establish the cause of the reported coordinate miss.

Install or update through Honeycomb with `honeycomb install extend`, or install from crates.io
with `cargo install silicon-extend-cli --version 1.1.1 --locked`.
