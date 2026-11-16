#!/usr/bin/env python3
"""SOLO: Checks whether Solo's agent screen-detection rules have fallen behind
Herdr's (https://github.com/herdrdev/herdr, Apache-2.0).

Solo bundles Herdr's `distribution/agent-detection/*.toml` in
`vendor/herdr_agent_detection/` and interprets them with a port of Herdr's
`src/detect/manifest.rs` (`crates/agent_detection`). This compares, against
Herdr's default branch (or `--herdr-dir`, a checkout):

- the rule files: added, removed or changed upstream;
- the rule language: Herdr's `MANIFEST_ENGINE_VERSION` against Solo's
  `ENGINE_VERSION`, and the regions and rule keys Herdr's interpreter accepts
  against the ones Solo's does.

It prints what differs (as a GitHub step summary too, when run in Actions)
and exits with 1 if anything does, 2 if it couldn't check.

    python3 script/check-herdr-rules.py [--herdr-dir <checkout>] [--ref <branch or commit>]
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERDR_REPO = "https://github.com/herdrdev/herdr.git"
RULES_DIR = "distribution/agent-detection"
ENGINE_FILE = "src/detect/manifest_update.rs"
INTERPRETER_FILE = "src/detect/manifest.rs"

ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor" / "herdr_agent_detection"
SOLO_ENGINE = ROOT / "crates" / "agent_detection" / "src" / "agent_detection.rs"


def fetch_herdr(ref: str | None, into: Path) -> Path:
    """A shallow, sparse checkout of the files this compares."""
    checkout = into / "herdr"
    clone = ["git", "clone", "--quiet", "--depth", "1", "--filter=blob:none", "--sparse"]
    if ref:
        clone += ["--branch", ref]
    subprocess.run(clone + [HERDR_REPO, str(checkout)], check=True)
    subprocess.run(
        ["git", "-C", str(checkout), "sparse-checkout", "set", "--no-cone",
         f"/{RULES_DIR}/", f"/{ENGINE_FILE}", f"/{INTERPRETER_FILE}"],
        check=True,
    )
    return checkout


def head_commit(checkout: Path) -> str:
    try:
        return subprocess.run(
            ["git", "-C", str(checkout), "rev-parse", "HEAD"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def read(path: Path) -> str:
    # Line endings may differ with git's autocrlf.
    return path.read_text(encoding="utf-8").replace("\r\n", "\n")


def rule_files(directory: Path) -> dict[str, str]:
    return {path.name: read(path) for path in sorted(directory.glob("*.toml"))}


def int_const(text: str, name: str) -> int | None:
    match = re.search(rf"const {name}: u32 = (\d+);", text)
    return int(match.group(1)) if match else None


def function_body(text: str, signature: str) -> str:
    start = text.find(signature)
    if start < 0:
        return ""
    depth = 0
    for index in range(text.index("{", start), len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return text[start:index]
    return text[start:]


def herdr_regions(interpreter: str) -> set[str]:
    body = function_body(interpreter, "fn validate_region_name")
    # Counted regions are checked by prefix: region_count(trimmed, "bottom_lines").
    counted = re.findall(r'region_count\(trimmed, "([a-z_]+)"\)', body)
    names = set(re.findall(r'"([a-z_]+)"', body)) - set(counted)
    names |= {f"{name}(n)" for name in counted}
    if "top_region_count" in body:
        names.add("top_non_empty_lines(n)")
    return names


def solo_regions(engine: str) -> set[str]:
    body = function_body(engine, "fn parse(text: &str) -> Result<Self>")
    names = {name for name in re.findall(r'"([a-z_]+)" =>', body)}
    names |= {f"{name}(n)" for name in re.findall(r'region_count\(text, "([a-z_]+)"\)', body)}
    return names


def struct_keys(text: str, struct: str) -> set[str]:
    match = re.search(rf"struct {struct} \{{(.*?)\n\}}", text, re.S)
    if not match:
        return set()
    keys = set()
    rename = None
    for line in match.group(1).splitlines():
        line = line.strip()
        renamed = re.search(r'rename = "([a-z_]+)"', line)
        if line.startswith("#["):
            if renamed:
                rename = renamed.group(1)
            continue
        field = re.match(r"(?:pub(?:\([a-z]+\))? )?([a-z_]+):", line)
        if field:
            keys.add(rename or field.group(1).lstrip("_"))
            rename = None
    return keys


def compare(herdr: Path) -> list[str]:
    problems = []
    upstream = rule_files(herdr / RULES_DIR)
    vendored = rule_files(VENDOR)
    if not upstream:
        raise RuntimeError(f"no rule files in {herdr / RULES_DIR}")
    for name in sorted(upstream.keys() - vendored.keys()):
        problems.append(f"new upstream rule file: {name}")
    for name in sorted(vendored.keys() - upstream.keys()):
        problems.append(f"rule file removed upstream: {name}")
    for name in sorted(upstream.keys() & vendored.keys()):
        if upstream[name] != vendored[name]:
            version = re.search(r'^version = "([^"]+)"', upstream[name], re.M)
            ours = re.search(r'^version = "([^"]+)"', vendored[name], re.M)
            problems.append(
                f"changed upstream: {name}"
                f" (Solo {ours.group(1) if ours else '?'}, Herdr {version.group(1) if version else '?'})"
            )

    engine = read(SOLO_ENGINE)
    herdr_engine = int_const(read(herdr / ENGINE_FILE), "MANIFEST_ENGINE_VERSION")
    solo_engine = int_const(engine, "ENGINE_VERSION")
    if herdr_engine is None or solo_engine is None:
        problems.append("couldn't read the engine versions (did a file move?)")
    elif herdr_engine != solo_engine:
        problems.append(f"engine version: Herdr {herdr_engine}, Solo {solo_engine}")

    interpreter = read(herdr / INTERPRETER_FILE)
    theirs, ours = herdr_regions(interpreter), solo_regions(engine)
    if not theirs or not ours:
        problems.append("couldn't read the region names (did the code change shape?)")
    for region in sorted(theirs - ours):
        problems.append(f"region Solo doesn't support: {region}")
    for upstream_struct, solo_struct in [
        ("AgentManifest", "ManifestFile"),
        ("ManifestRule", "RuleFile"),
        ("ManifestGate", "GateFile"),
    ]:
        their_keys = struct_keys(interpreter, upstream_struct)
        our_keys = struct_keys(engine, solo_struct)
        if not their_keys or not our_keys:
            problems.append(f"couldn't read the keys of {upstream_struct}")
        for key in sorted(their_keys - our_keys):
            problems.append(f"rule key Solo doesn't support ({upstream_struct}): {key}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--herdr-dir", type=Path, help="a Herdr checkout to compare with")
    parser.add_argument("--ref", help="the Herdr branch or tag to fetch (default: its default branch)")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        try:
            herdr = args.herdr_dir or fetch_herdr(args.ref, Path(temp))
            commit = head_commit(herdr)
            problems = compare(herdr)
        except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
            print(f"couldn't check the rules: {error}", file=sys.stderr)
            return 2
    revision_file = VENDOR / "REVISION"
    vendored_at = read(revision_file).strip() if revision_file.exists() else "?"
    if problems:
        lines = [
            f"Solo's agent detection rules (Herdr {vendored_at[:12]}) are behind Herdr {commit[:12]}:",
            *[f"- {problem}" for problem in problems],
            "",
            "Copy the changed files from Herdr's distribution/agent-detection into "
            "vendor/herdr_agent_detection, update REVISION and README.md there and in "
            "vendor/README.md, and port any engine change to crates/agent_detection.",
        ]
    else:
        lines = [f"Solo's agent detection rules match Herdr {commit[:12]}."]
    report = "\n".join(lines)
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as file:
            file.write(report + "\n")
    if problems and os.environ.get("GITHUB_ACTIONS"):
        print(f"::warning title=Agent detection rules behind Herdr::{len(problems)} difference(s); see the job summary")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
