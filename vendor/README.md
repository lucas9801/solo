# Vendored crates

Patched copies of upstream crates that Solo needs to change.

| Crate | Upstream | Base | Why |
|---|---|---|---|
| `alacritty_terminal` | github.com/zed-industries/alacritty | rev `4c129667ce56611becdc82de6e28218c80e2e88f` | Forward shell integration sequences (OSC 133, OSC 7) as terminal events |
| `vte` | crates.io `vte` | `0.15.0` | Pass OSC sequences the ANSI processor doesn't handle on to the `Handler` |

Search for `SOLO:` to find every local change.
- `alacritty_terminal/tests` (46 MB of reference grids) is not vendored; run those tests upstream.
