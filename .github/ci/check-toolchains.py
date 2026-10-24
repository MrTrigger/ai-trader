#!/usr/bin/env python3
"""Check the repository's toolchain pins without installing build tools."""

import argparse
import fnmatch
import json
from pathlib import Path
import re
import subprocess
import sys


RULES = [
    ("node", r"\bnode[-_]version\s*[:=]\s*['\"]?(?P<version>\d+(?:\.\d+){0,2}(?:\.x)?|lts/\*|latest|current)"),
    ("node", r"\bNODE_VERSION\s*=\s*['\"]?(?P<version>\d+(?:\.\d+){0,2})"),
    ("node", r"\bnode-v(?P<version>\d+\.\d+\.\d+)"),
    ("bun", r"\bbun-version\s*:\s*['\"]?(?P<version>\d+(?:\.\d+){0,2}|latest|canary)"),
    ("bun", r"\bBUN_VERSION\s*=\s*['\"]?(?P<version>\d+(?:\.\d+){0,2})"),
    ("bun", r"\bbun-v(?P<version>\d+\.\d+\.\d+)"),
    ("pnpm", r"\bpnpm@(?P<version>\d+\.\d+\.\d+)"),
    ("pnpm", r"\bPNPM_VERSION\s*=\s*['\"]?(?P<version>\d+\.\d+\.\d+)"),
    ("bun", r"\bbun@(?P<version>\d+\.\d+\.\d+)"),
    ("node", r"\bnode:(?P<version>\d+(?:\.\d+){0,2})"),
    ("bun", r"\boven/bun:(?P<version>\d+(?:\.\d+){0,2})"),
]
PATTERNS = [(tool, re.compile(pattern)) for tool, pattern in RULES]
VERSION_FILES = {".node-version": "node", ".nvmrc": "node", ".bun-version": "bun", ".bazelversion": "bazel"}


def candidates(root):
    result = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=root, text=True, capture_output=True, check=True,
    )
    for name in sorted(set(result.stdout.split("\0")) - {""}):
        path = Path(name)
        if path.parts[0] in {"vendor", "third_party", "node_modules", ".worktrees", "docs"}:
            continue
        if name.startswith(".github/ci/"):
            continue
        if (path.name in VERSION_FILES or path.name in {"MODULE.bazel", "package.json", ".mise.toml", "mise.toml", ".tool-versions"}
                or "Dockerfile" in path.name or name.startswith((".github/workflows/", ".github/actions/"))):
            if (root / name).is_file() and not (root / name).is_symlink():
                yield name


def occurrences(name, content):
    if Path(name).name in VERSION_FILES:
        match = re.search(r"\S+", content)
        if match:
            yield VERSION_FILES[Path(name).name], match.start(), match.end(), match.group()
        return
    offset = 0
    pnpm_step_indent = None
    for line in content.splitlines(keepends=True):
        depth = len(line) - len(line.lstrip())
        if line.strip() and pnpm_step_indent is not None and depth <= pnpm_step_indent:
            pnpm_step_indent = None
        if not line.lstrip().startswith(("#", "//")):
            if "uses: pnpm/action-setup@" in line:
                pnpm_step_indent = depth if line.lstrip().startswith("-") else depth - 2
            if pnpm_step_indent is not None:
                match = re.search(r"\bversion:\s*['\"]?([\w.*-]+)", line)
                if match:
                    yield "pnpm", offset + match.start(1), offset + match.end(1), match.group(1)
            for tool, pattern in PATTERNS:
                for match in pattern.finditer(line):
                    yield tool, offset + match.start("version"), offset + match.end("version"), match.group("version")
            if Path(name).name in {".mise.toml", "mise.toml", ".tool-versions"}:
                match = re.match(r"\s*(node|bun|pnpm)\s*(?:=\s*[\"']|\s)([\d.]+)", line)
                if match:
                    yield match.group(1), offset + match.start(2), offset + match.end(2), match.group(2)
            matrix = re.search(r"\bnode-version:\s*\[([^\]]*)\]", line)
            if matrix:
                for match in re.finditer(r"[\w./*+-]+", matrix.group(1)):
                    start = offset + matrix.start(1) + match.start()
                    yield "node", start, start + len(match.group()), match.group()
        offset += len(line)


def inspect(root, policy):
    pins, errors = [], []
    for name in candidates(root):
        excluded = next((reason for pattern, reason in policy.get("exclude", {}).items() if fnmatch.fnmatchcase(name, pattern)), None)
        if excluded:
            continue
        content = (root / name).read_text()
        for tool, start, end, value in occurrences(name, content):
            expected = policy["versions"][tool]
            line = content.count("\n", 0, start) + 1
            pins.append({"path": name, "line": line, "tool": tool, "version": value})
            if value != expected:
                errors.append(f"{name}:{line}: {tool} is {value}; shared policy requires {expected}")
    return pins, errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--policy", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    policy_path = args.policy or args.root / ".github/ci/toolchains.json"
    policy = json.loads(policy_path.read_text())
    if policy.get("schema") != 1 or set(policy["versions"]) != {"node", "bun", "pnpm", "bazel"}:
        raise SystemExit("Unsupported toolchain policy; refresh it with dotfiles/scripts/ci-fleet.py sync")
    pins, errors = inspect(args.root, policy)
    if args.json:
        print(json.dumps({"repository": policy["repository"], "pins": pins, "errors": errors}, indent=2))
    else:
        for error in errors:
            print(error, file=sys.stderr)
        print(f"Checked {len(pins)} toolchain pins for {policy['repository']}; {len(errors)} differences")
        if errors:
            print("Update ci/fleet.json in dotfiles, run scripts/ci-fleet.py sync, and validate the affected builds.", file=sys.stderr)
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
