# herdr_agent_detection

Agent screen-detection rules from [Herdr](https://github.com/herdrdev/herdr),
commit `ee5bfb5041bb3e4a49d591c72308aae3c81ecdde`, directory
`distribution/agent-detection/` (`index.toml` plus one TOML manifest per
agent). Apache-2.0 licensed, see `LICENSE`.

The files are unmodified copies of upstream. Herdr ships identical copies in
`src/detect/manifests/`, which it embeds as the built-in defaults.

Solo's interpreter for these rules is a port of Herdr's `src/detect/manifest.rs`.

`REVISION` holds the upstream commit. `script/check-herdr-rules.py` compares
these files, and the rule language Solo's interpreter accepts (engine version,
regions, rule keys), with Herdr's latest; the `Solo agent rules vs Herdr`
workflow runs it weekly and opens an issue when Solo is behind.

`crates/agent_detection/test_screens/` holds the screens of common agents the
bundled rules are tested on (`accuracy_tests.rs`).
