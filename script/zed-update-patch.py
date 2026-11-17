#!/usr/bin/env python3
"""Write Zed's changes between two commits as a patch for Solo (UPSTREAM.md).

    python script/zed-update-patch.py --zed ../solo-zed-upstream --base <sha> --new <rev> [-o zed.patch]

Leaves out the files Solo removed from Zed (in Zed at --base, not in Solo's
HEAD), the files Zed added under a directory Solo removed entirely (in Zed at
--base, with nothing left of it in Solo's HEAD), and Zed's own agent rules
(.rules, CLAUDE.md, AGENTS.md), and lists what it left out. Apply the patch
with `git apply -3 --index zed.patch`.
"""
import argparse
import posixpath
import subprocess
import sys

ZED_ONLY = {".rules", "CLAUDE.md", "AGENTS.md"}


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, check=True).stdout


def tree_paths(repo, rev):
    out = git(repo, "ls-tree", "-r", "-z", "--name-only", "--full-tree", rev)
    return {p.decode("utf-8") for p in out.split(b"\0") if p}


def parent_dirs(paths):
    dirs = set()
    for path in paths:
        parent = posixpath.dirname(path)
        while parent and parent not in dirs:
            dirs.add(parent)
            parent = posixpath.dirname(parent)
    return dirs


def removed_dir_of(path, removed_dirs):
    """The outermost directory of `path` that Solo removed entirely, if any."""
    parts = path.split("/")[:-1]
    for i in range(1, len(parts) + 1):
        candidate = "/".join(parts[:i])
        if candidate in removed_dirs:
            return candidate
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--zed", required=True, help="a clone of Zed (blobless is enough)")
    parser.add_argument("--base", required=True, help="the Zed commit in UPSTREAM.md")
    parser.add_argument("--new", required=True, help="the Zed commit or tag to update to")
    parser.add_argument("--solo", default=".", help="the Solo checkout (default: here)")
    parser.add_argument("-o", "--output", default="zed.patch")
    args = parser.parse_args()

    base_paths = tree_paths(args.zed, args.base)
    solo_paths = tree_paths(args.solo, "HEAD")
    removed = base_paths - solo_paths
    # Directories Zed had at the base of which Solo kept nothing.
    removed_dirs = parent_dirs(base_paths) - parent_dirs(solo_paths)
    patch = git(args.zed, "diff", "--binary", "--full-index", "--no-renames", args.base, args.new)

    kept, left_out, new_in_removed = [], [], []
    for i, block in enumerate(patch.split(b"\ndiff --git ")):
        text = block if i == 0 else b"diff --git " + block
        if not text.strip():
            continue
        header = text.split(b"\n", 1)[0].decode("utf-8")
        path = header.split(" b/", 1)[0].removeprefix("diff --git a/")
        if path in removed or path in ZED_ONLY:
            left_out.append(path)
        elif path not in base_paths and removed_dir_of(path, removed_dirs):
            new_in_removed.append(path)
        else:
            kept.append(text)
    with open(args.output, "wb") as f:
        f.write(b"\n".join(kept) + (b"\n" if kept and not kept[-1].endswith(b"\n") else b""))

    print(f"{args.output}: {len(kept)} files; left out {len(left_out)} that Solo removed or that are Zed's own:")
    for path in left_out:
        print("  " + path)
    print(f"and {len(new_in_removed)} that Zed added under directories Solo removed:")
    for path in new_in_removed:
        print("  " + path)


if __name__ == "__main__":
    sys.exit(main())
