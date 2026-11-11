#!/usr/bin/env python3
"""Compare Solo's actions with docs/testing/feature-checklist.md.

Lists every action declared in crates/ (the `actions!` macro and
`#[derive(Action)]` with `#[action(namespace = ...)]`), drops the ones Zed
already had (script/solo-feature-audit/upstream-actions.txt), and reports
which of the rest the checklist doesn't mention as `namespace::Name`, and
which actions the checklist mentions that no longer exist.

    python script/solo-feature-audit.py                 # report, exit 1 on gaps
    python script/solo-feature-audit.py --list          # Solo's actions
    python script/solo-feature-audit.py --list --all    # every action
    python script/solo-feature-audit.py --write-baseline zed-v1.27.0

`--write-baseline REV` regenerates the upstream list from a Zed tag or
commit (needs the `upstream` remote's history), e.g. after merging Zed.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "script" / "solo-feature-audit" / "upstream-actions.txt"
CHECKLIST = ROOT / "docs" / "testing" / "feature-checklist.md"

ACTIONS_MACRO = re.compile(r"\bactions!\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*\[", re.S)
ACTION_ATTR = re.compile(r"#\[action\(((?:[^\[\]]|\[[^\]]*\])*)\)\]")
ITEM = re.compile(r"\b(?:struct|enum)\s+(\$?[A-Za-z_][A-Za-z0-9_]*)")
MACRO_RULES = re.compile(r"macro_rules!\s*([A-Za-z_][A-Za-z0-9_]*)")
ALIASES = re.compile(r"deprecated_aliases\s*=\s*\[([^\]]*)\]")
NAME_FOR_TYPE = re.compile(r"fn\s+name_for_type\(\)[^{]*\{\s*\"([a-z_][a-z0-9_]*::[A-Za-z0-9_]+)\"")
REFERENCE = re.compile(r"`([a-z_][a-z0-9_]*)::([A-Z][A-Za-z0-9_]*)`")


def strip_comments(text):
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return re.sub(r"//[^\n]*", "", text)


def bracket_body(text, start):
    """The text between `[` at `start - 1` and its matching `]`."""
    depth = 1
    index = start
    while index < len(text) and depth:
        if text[index] == "[":
            depth += 1
        elif text[index] == "]":
            depth -= 1
        index += 1
    return text[start : index - 1]


def actions_in(text):
    text = strip_comments(text)
    found = set()
    # Registered too: old names that still work in keymaps, and actions
    # that implement the trait by hand.
    for match in ALIASES.finditer(text):
        found.update(re.findall(r'"([^"]+)"', match.group(1)))
    found.update(NAME_FOR_TYPE.findall(text))
    for match in ACTIONS_MACRO.finditer(text):
        namespace = match.group(1)
        body = bracket_body(text, match.end())
        # Attributes such as #[action(deprecated_aliases = [...])] sit
        # between the names; #[action(name = "...")] renames the next one.
        renamed = None
        for token in re.finditer(
            r"#\[[^\[\]]*(?:\[[^\]]*\][^\[\]]*)*\]|[A-Za-z_][A-Za-z0-9_]*", body
        ):
            token = token.group(0)
            if token.startswith("#"):
                name = re.search(r'\bname\s*=\s*"([^"]+)"', token)
                renamed = name.group(1) if name else renamed
                continue
            found.add(f"{namespace}::{renamed or token}")
            renamed = None
    for match in ACTION_ATTR.finditer(text):
        arguments = match.group(1)
        namespace = re.search(r"namespace\s*=\s*([A-Za-z_][A-Za-z0-9_]*)", arguments)
        if not namespace:
            continue
        item = ITEM.search(text, match.end())
        if not item:
            continue
        if item.group(1).startswith("$"):
            # A macro that declares an action for each of its arguments,
            # written `Name => ...` where it's used.
            definitions = list(MACRO_RULES.finditer(text, 0, match.start()))
            if definitions:
                macro = definitions[-1].group(1)
                for use in re.finditer(rf"\b{macro}!\s*\(", text):
                    body = text[use.end() : text.find(");", use.end())]
                    for name in re.findall(r"\b([A-Z][A-Za-z0-9_]*)\s*=>", body):
                        found.add(f"{namespace.group(1)}::{name}")
            continue
        name = re.search(r'\bname\s*=\s*"([^"]+)"', arguments)
        found.add(f"{namespace.group(1)}::{name.group(1) if name else item.group(1)}")
    return found


def worktree_actions():
    actions = set()
    for path in (ROOT / "crates").rglob("*.rs"):
        actions |= actions_in(path.read_text(encoding="utf-8", errors="replace"))
    return actions


def revision_actions(revision):
    listing = subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", revision, "--", "crates"],
        cwd=ROOT, capture_output=True, text=True, check=True,
    ).stdout.split()
    paths = [path for path in listing if path.endswith(".rs")]
    request = "".join(f"{revision}:{path}\n" for path in paths).encode()
    output = subprocess.run(
        ["git", "cat-file", "--batch"], cwd=ROOT, input=request,
        capture_output=True, check=True,
    ).stdout
    actions = set()
    position = 0
    while position < len(output):
        header_end = output.index(b"\n", position)
        header = output[position:header_end].split()
        if len(header) < 3:
            # "<name> missing": a blob a partial clone couldn't fetch.
            sys.exit(f"git can't read {header[0].decode()}")
        size = int(header[2])
        content = output[header_end + 1 : header_end + 1 + size]
        position = header_end + 1 + size + 1
        actions |= actions_in(content.decode("utf-8", errors="replace"))
    return actions


def read_baseline():
    if not BASELINE.exists():
        sys.exit(f"missing {BASELINE}; run with --write-baseline <zed tag>")
    return {
        line.strip()
        for line in BASELINE.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.startswith("#")
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--list", action="store_true", help="print actions and exit")
    parser.add_argument("--all", action="store_true", help="with --list: Zed's too")
    parser.add_argument("--write-baseline", metavar="REV")
    arguments = parser.parse_args()

    if arguments.write_baseline:
        actions = sorted(revision_actions(arguments.write_baseline))
        BASELINE.parent.mkdir(parents=True, exist_ok=True)
        with open(BASELINE, "w", encoding="utf-8", newline="\n") as file:
            file.write(f"# Actions in Zed {arguments.write_baseline}, by solo-feature-audit.py\n")
            file.writelines(f"{action}\n" for action in actions)
        print(f"wrote {len(actions)} actions to {BASELINE.relative_to(ROOT)}")
        return 0

    current = worktree_actions()
    baseline = read_baseline()
    # Solo renamed Zed's `zed` namespace to `solo`.
    baseline |= {
        "solo::" + action.removeprefix("zed::")
        for action in baseline
        if action.startswith("zed::")
    }
    solo = current - baseline
    if arguments.list:
        for action in sorted(current if arguments.all else solo):
            print(action)
        return 0

    checklist = CHECKLIST.read_text(encoding="utf-8")
    mentioned = {f"{namespace}::{name}" for namespace, name in REFERENCE.findall(checklist)}
    missing = sorted(solo - mentioned)
    # Only Solo's own namespaces: the checklist may also name Zed actions
    # with backticks, and code spans like `util::i18n` aren't actions.
    solo_namespaces = {action.split("::")[0] for action in solo}
    stale = sorted(
        action
        for action in mentioned - current
        if action.split("::")[0] in solo_namespaces
    )

    print(f"{len(solo)} Solo actions, {len(solo) - len(missing)} in the checklist")
    if missing:
        print("\nNot in docs/testing/feature-checklist.md:")
        for action in missing:
            print(f"  {action}")
    if stale:
        print("\nIn the checklist but not in the code:")
        for action in stale:
            print(f"  {action}")
    return 1 if missing or stale else 0


if __name__ == "__main__":
    sys.exit(main())
