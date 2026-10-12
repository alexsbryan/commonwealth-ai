#!/usr/bin/env python3
"""Fail when conformance.toml leaves an engine-boundary name uncovered.

The boundary is every field of CompletionRequest and CompletionResponse and
every method of InferenceProvider, read from the source, so a field added to
the request without a conformance row fails here rather than going untested.
"""
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
COMPLETION = ROOT / "shared/crates/oicp-types/src/completion.rs"
TRAITS = ROOT / "shared/crates/sovereign-contracts/src/traits.rs"
INVENTORY = Path(__file__).with_name("conformance.toml")


def struct_fields(src: str, name: str) -> list[str]:
    body = re.search(r"pub struct %s *\{(.*?)\n\}" % name, src, re.S)
    if body is None:
        sys.exit(f"coverage: struct {name} not found in {COMPLETION}")
    return re.findall(r"^\s+pub (\w+):", body.group(1), re.M)


def trait_methods(src: str) -> list[str]:
    body = re.search(r"pub trait InferenceProvider\b.*?\n\}", src, re.S)
    if body is None:
        sys.exit(f"coverage: trait InferenceProvider not found in {TRAITS}")
    return re.findall(r"fn (\w+)", body.group(0))


def main() -> int:
    inv = tomllib.loads(INVENTORY.read_text())
    covered = {c.split(":")[0] for r in inv["row"] + inv["unreachable"] for c in r["covers"]}
    completion = COMPLETION.read_text()
    boundary = {
        "CompletionRequest": struct_fields(completion, "CompletionRequest"),
        "CompletionResponse": struct_fields(completion, "CompletionResponse"),
        "InferenceProvider": trait_methods(TRAITS.read_text()),
    }
    missing = 0
    for owner, names in boundary.items():
        gaps = [n for n in names if n not in covered]
        missing += len(gaps)
        print(f"{owner}: {len(names) - len(gaps)}/{len(names)} covered" + (f", missing {gaps}" if gaps else ""))
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
