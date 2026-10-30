# Vendored crates

Patched copies of upstream crates that Solo needs to change.

| Crate | Upstream | Base | Why |
|---|---|---|---|
| `alacritty_terminal` | github.com/zed-industries/alacritty | rev `4c129667ce56611becdc82de6e28218c80e2e88f` | Forward shell integration sequences (OSC 133, OSC 633 E, OSC 7) as terminal events |
| `vte` | crates.io `vte` | `0.15.0` | Pass OSC sequences the ANSI processor doesn't handle on to the `Handler` |

Search for `SOLO:` to find every local change.

The vendored crates are excluded from the workspace. To run the patched
`alacritty_terminal` unit tests against the patched `vte`:

```sh
cd vendor/alacritty_terminal
cargo test --lib --config 'patch.crates-io.vte.path="../vte"'
```

`alacritty_terminal/tests` (46 MB of reference grids) is not vendored; run those
tests upstream.
