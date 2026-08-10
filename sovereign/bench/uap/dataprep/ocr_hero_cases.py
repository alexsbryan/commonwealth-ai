#!/usr/bin/env python3
"""Hero-set OCR driver: for each unidentified (is_unidentified) NARA Blue
Book case, download the first N page JPGs (the Project-10073 Record Card,
which carries the disposition + summary), run them through the headless
PaddleOCR example, assemble the per-case narrative, and emit cases.jsonl.

Resumable: skips cases already in the output; caches downloaded images.
Disposition is UNIDENTIFIED by construction (these are the AF's own
"Unknown" rulings, joined from NICAP) — the ground-truth label.

Run AFTER building the OCR binary:
  cargo build --release --example ocr_images --features paddle-ocr -p sovereign-tools
"""
import argparse
import json
import os
import re
import subprocess
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
# dataprep -> uap -> bench -> sovereign -> <repo root> (target/ lives here)
REPO = os.path.abspath(os.path.join(HERE, "..", "..", "..", ".."))
OCR_BIN = next(
    (p for p in (
        os.path.join(REPO, "target", "release", "examples", "ocr_images"),
        os.path.join(os.getcwd(), "target", "release", "examples", "ocr_images"),
    ) if os.path.exists(p)),
    os.path.join(REPO, "target", "release", "examples", "ocr_images"),
)
IMG_CACHE = os.path.expanduser("~/.svrnmesh/corpora-staging/uap-hero-images")


def download(url: str, dest: str) -> bool:
    if os.path.exists(dest) and os.path.getsize(dest) > 0:
        return True
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "sovereign-uap-dataprep/0.1"})
        with urllib.request.urlopen(req, timeout=120) as r, open(dest + ".part", "wb") as f:
            f.write(r.read())
        os.replace(dest + ".part", dest)
        return True
    except Exception as e:
        sys.stderr.write(f"  download fail {url}: {e}\n")
        return False


def ocr(paths: list[str]) -> dict[str, str]:
    """Run the OCR binary over image paths; return path -> text."""
    if not paths:
        return {}
    out = subprocess.run([OCR_BIN, *paths], capture_output=True, text=True, timeout=900)
    texts, cur_path, buf = {}, None, []
    for line in out.stdout.splitlines():
        m = re.match(r"^<<<IMAGE (.+)>>>$", line)
        if m:
            if cur_path is not None:
                texts[cur_path] = "\n".join(buf).strip()
            cur_path, buf = m.group(1), []
        elif cur_path is not None:
            buf.append(line)
    if cur_path is not None:
        texts[cur_path] = "\n".join(buf).strip()
    return texts


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--pages", type=int, default=3, help="first N page images per case")
    ap.add_argument("--limit", type=int, default=0, help="0 = all hero cases")
    ap.add_argument("--out", default=os.path.join(HERE, "cases_real.jsonl"))
    args = ap.parse_args()

    if not os.path.exists(OCR_BIN):
        sys.stderr.write(f"OCR binary missing: {OCR_BIN}\n  build it first (see module docstring)\n")
        return 1
    os.makedirs(IMG_CACHE, exist_ok=True)

    hero = [json.loads(l) for l in open(os.path.join(HERE, "metadata.jsonl")) if l.strip()]
    hero = [r for r in hero if r.get("is_unidentified") and r.get("n_images", 0) > 0]
    if args.limit:
        hero = hero[: args.limit]

    done = set()
    if os.path.exists(args.out):
        for l in open(args.out):
            try:
                done.add(json.loads(l)["naId"])
            except Exception:
                pass

    written = len(done)
    with open(args.out, "a") as out:
        for i, case in enumerate(hero):
            naid = case["naId"]
            if naid in done:
                continue
            case_dir = os.path.join(IMG_CACHE, str(naid))
            os.makedirs(case_dir, exist_ok=True)
            local = []
            for j, url in enumerate(case["image_urls"][: args.pages]):
                dest = os.path.join(case_dir, f"p{j:02d}.jpg")
                if download(url, dest):
                    local.append(dest)
            texts = ocr(local)
            narrative = "\n\n".join(texts[p] for p in local if texts.get(p)).strip()
            if not narrative:
                sys.stderr.write(f"  [{i}] naId {naid}: no OCR text, skipping\n")
                continue
            rec = {
                "case_id": f"BB-{case.get('nicap_case_no') or naid}",
                "naId": naid,
                "nicap_case_no": case.get("nicap_case_no"),
                "date": case.get("date"),
                "location": case.get("location"),
                "disposition": "UNIDENTIFIED",
                "narrative": narrative,
                "n_pages_ocrd": len(local),
                "source": "nara_aws_odr+paddleocr",
            }
            out.write(json.dumps(rec) + "\n")
            out.flush()
            written += 1
            if written % 10 == 0:
                sys.stderr.write(f"  ...{written} cases OCR'd (at index {i}/{len(hero)})\n")

    print(f"hero cases with OCR narrative: {written} → {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
