#!/usr/bin/env python3
"""Concatenate <src>/<service>/*.md into one markdown file (the `markdown`
extractor reads ONE file). Each source doc becomes `# <Service> — <Doc>`;
its own headings (setext or ATX) are demoted one level under it."""
import argparse, re, pathlib
ROOT = pathlib.Path(__file__).parent


def demote(lines):
    """One source doc's lines with its headings demoted one level."""
    conv = []
    i = 0
    in_fence = False
    while i < len(lines):
        ln = lines[i]
        if ln.lstrip().startswith("```"):
            in_fence = not in_fence
        nxt = lines[i + 1] if i + 1 < len(lines) else ""
        if not in_fence and ln.strip() and re.fullmatch(r"=+\s*", nxt):
            conv.append("## " + ln.strip()); i += 2; continue
        if not in_fence and ln.strip() and re.fullmatch(r"-{3,}\s*", nxt) and not ln.startswith(("*", "-", "|", " ")):
            conv.append("### " + ln.strip()); i += 2; continue
        m = None if in_fence else re.match(r"^(#{1,6})\s+(.*)$", ln)
        if m:
            conv.append("#" * min(6, len(m.group(1)) + 1) + " " + m.group(2))
        else:
            conv.append(ln)
        i += 1
    return conv


def build(services, src, out_path):
    out = []
    for svc in services:
        for f in sorted((src / svc).glob("*.md")):
            out.append(f"# {svc} — {f.stem}\n\nService: {svc}. Document: {svc} {f.stem}.\n")
            out.append("\n".join(demote(f.read_text().splitlines())) + "\n")
    out_path.write_text("\n".join(out))


def self_test():
    conv = demote(["Data we collect", "===============", "## Sharing", "```", "# not a heading", "```"])
    assert conv[0] == "## Data we collect", conv      # setext h1 -> ATX h2
    assert conv[1] == "### Sharing", conv             # ATX h2 -> h3
    assert conv[3] == "# not a heading", conv         # fenced `#` left alone
    print("self-test: passed")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--services", default="Reddit,Spotify,LinkedIn")
    ap.add_argument("--src", type=pathlib.Path, default=ROOT / "corpus")
    ap.add_argument("--out", type=pathlib.Path, default=ROOT / "fineprint.md")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        self_test()
    else:
        build([s.strip() for s in a.services.split(",")], a.src, a.out)
