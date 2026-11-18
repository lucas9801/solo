# Upstream: Zed

Solo is based on [Zed](https://github.com/zed-industries/zed). The first
commit of this repository is Zed's source, unchanged except that Zed's own
agent rules (`.rules`, `CLAUDE.md`, `AGENTS.md`) are left out; every later
commit is Solo's.

| | |
|---|---|
| Zed commit | [`4d847859f5506471bd80ca78ee6ced161b19a4e8`](https://github.com/zed-industries/zed/commit/4d847859f5506471bd80ca78ee6ced161b19a4e8) |
| Date | 2026-10-28 (Zed's `main`) |
| Zed version | 1.28.0 (Zed's `crates/zed/Cargo.toml`) |
| Imported in | the first commit |

When Zed is updated, change the table to the new commit.

## Merging a Zed update

Zed's changes since the base commit are applied as a patch, with a
three-way merge where they meet Solo's changes. In PowerShell, in the Solo
checkout, with a clone of Zed next to it (a blobless clone is enough:
`git clone --filter=blob:none --bare https://github.com/zed-industries/zed.git ..\solo-zed-upstream`):

```powershell
$zed  = "..\solo-zed-upstream"
$base = "4d847859f5506471bd80ca78ee6ced161b19a4e8"   # the table above
git -C $zed fetch origin --tags
$new  = git -C $zed rev-parse origin/main              # or a release tag, e.g. v1.29.0

# Zed's changes, without the files Solo removed from Zed, the files Zed
# added under directories Solo removed entirely (such as crates/collab), and
# Zed's own agent rules (.rules, CLAUDE.md, AGENTS.md). It lists what it
# left out. Paths Solo renamed (crates/zed -> crates/solo, zed_actions ->
# solo_actions, and the others in the script's RENAMED list) are written
# under Solo's names.
python script/zed-update-patch.py --zed $zed --base $base --new $new -o ..\zed.patch

# The three-way merge needs Zed's version of each file at the base: the
# first commit has it for the first update; after that, fetch it.
git fetch --no-tags --depth 1 https://github.com/zed-industries/zed.git $base

git switch -c upstream-update
git apply -3 --index ..\zed.patch
```

Then:

1. `git status` shows conflicts as `UU`. Resolve them keeping both Zed's
   change and Solo's (Solo's changes to Zed's code are marked `// SOLO:`).
   For `Cargo.lock`, take Zed's version and let cargo bring it up to date:
   `git checkout --theirs Cargo.lock`, then `cargo check -p solo --bin solo`.
2. New files (`A`) Zed added for things Solo removed but whose directory
   Solo still has (its own workflows in `.github/workflows`, bundling
   scripts in `script/`) can be removed with `git rm`.
3. Build and test as for any change, and fix Solo's code where Zed's APIs
   changed.
4. Change the table above to `$new`, its date and the version in Zed's
   `crates/zed/Cargo.toml`, and commit it all as one commit,
   "Update the editor base to <version>". The upstream commit is recorded
   only in the table above; commit messages, merge messages, tag messages
   and branch names never name the upstream editor (CI checks this with
   script/check-commit-messages.ps1).

The patch and the shallow fetch are only needed while merging; nothing from
them is pushed except the commit.
