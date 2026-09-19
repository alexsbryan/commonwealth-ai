#!/usr/bin/env python3
"""Concatenate corpus/<service>/*.md into one markdown file (the `markdown`
extractor reads ONE file). Each source doc becomes `# <Service> — <Doc>`;
its own headings (setext or ATX) are demoted one level under it."""
import re, pathlib
root = pathlib.Path(__file__).parent
out = []
for svc in ["Reddit", "Spotify", "LinkedIn"]:
    for f in sorted((root / "corpus" / svc).glob("*.md")):
        lines = f.read_text().splitlines()
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
        out.append(f"# {svc} — {f.stem}\n\nService: {svc}. Document: {svc} {f.stem}.\n")
        out.append("\n".join(conv) + "\n")
(root / "fineprint.md").write_text("\n".join(out))
