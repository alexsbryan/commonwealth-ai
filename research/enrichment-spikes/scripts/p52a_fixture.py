#!/usr/bin/env python3
"""P5.2a fixture builder (gate G9).

Builds a 16-page PDF where page i renders the opening of one SEP article, the
article being the primary expected_source of one sep-bench question. That
question is the page's query — real human-authored essay prompts, labels by
construction. Pages are then rasterized to PNG via pypdfium2 (the same PDFium
engine the production PdfiumRasterizer binds; rasterize.rs wiring exists but
needs the Tauri-bundled dylib, so the probe uses the Python binding).

Outputs under data/p52a/: fixture.pdf, pages/page_NN.png, labels.json.

Usage:
  .venv/bin/python scripts/p52a_fixture.py \
    --questions ~/dev/commonwealth-ai/sovereign/bench/sep/questions.toml \
    --parquet ~/.svrnmesh/indexes/_downloads/sep.parquet \
    --out-dir data/p52a --pages 16
"""

import argparse
import json
import tomllib
from collections import defaultdict
from pathlib import Path

import pyarrow.parquet as pq
import pypdfium2 as pdfium
from reportlab.lib.pagesizes import letter
from reportlab.lib.styles import ParagraphStyle
from reportlab.lib.units import inch
from reportlab.platypus import Paragraph, SimpleDocTemplate, Spacer

CHARS_PER_PAGE = 3200
DPI = 150


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--questions", required=True)
    ap.add_argument("--parquet", required=True)
    ap.add_argument("--out-dir", default="data/p52a")
    ap.add_argument("--pages", type=int, default=16)
    args = ap.parse_args()

    out = Path(args.out_dir)
    (out / "pages").mkdir(parents=True, exist_ok=True)

    with open(Path(args.questions).expanduser(), "rb") as f:
        bank = tomllib.load(f)
    questions = bank["questions"]

    # One page per distinct primary expected source, in bank order.
    chosen = []  # (qid, question, slug)
    used_slugs = set()
    for q in questions:
        sources = q.get("expected_sources", [])
        if not sources:
            continue
        slug = sources[0].lower()
        if slug in used_slugs:
            continue
        used_slugs.add(slug)
        chosen.append((q["id"], q["question"], slug))
        if len(chosen) >= args.pages:
            break
    print(f"pages: {len(chosen)}")

    texts: dict[str, list[str]] = defaultdict(list)
    pf = pq.ParquetFile(Path(args.parquet).expanduser())
    for batch in pf.iter_batches(columns=["category", "text"]):
        for slug, text in zip(
            batch.column("category").to_pylist(), batch.column("text").to_pylist()
        ):
            if slug and slug.lower() in used_slugs and text:
                texts[slug.lower()].append(text)

    # Render: one page per article — title + opening text, hard page break.
    pdf_path = out / "fixture.pdf"
    doc = SimpleDocTemplate(
        str(pdf_path), pagesize=letter,
        leftMargin=0.75 * inch, rightMargin=0.75 * inch,
        topMargin=0.75 * inch, bottomMargin=0.75 * inch,
    )
    title_style = ParagraphStyle("t", fontName="Times-Bold", fontSize=16, leading=20)
    body_style = ParagraphStyle("b", fontName="Times-Roman", fontSize=9, leading=11.5)
    from reportlab.platypus import PageBreak

    story = []
    labels = []
    for i, (qid, question, slug) in enumerate(chosen):
        body = " ".join(texts[slug])[:CHARS_PER_PAGE]
        body = body.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
        title = slug.replace("-", " ").title()
        story.append(Paragraph(title, title_style))
        story.append(Spacer(1, 10))
        # Split into paragraphs on sentence boundaries for a natural layout.
        chunk = ""
        for sent in body.split(". "):
            chunk += sent + ". "
            if len(chunk) > 700:
                story.append(Paragraph(chunk, body_style))
                story.append(Spacer(1, 5))
                chunk = ""
        if chunk.strip():
            story.append(Paragraph(chunk, body_style))
        if i < len(chosen) - 1:
            story.append(PageBreak())
        labels.append({"page": i, "slug": slug, "question_id": qid, "query": question})
    doc.build(story)
    print(f"wrote {pdf_path}")

    # Rasterize (PDFium — same engine as the production PdfiumRasterizer).
    pdf = pdfium.PdfDocument(str(pdf_path))
    assert len(pdf) == len(chosen), f"page count {len(pdf)} != {len(chosen)}"
    for i, page in enumerate(pdf):
        bitmap = page.render(scale=DPI / 72)
        bitmap.to_pil().save(out / "pages" / f"page_{i:02d}.png")
    print(f"rasterized {len(pdf)} pages at {DPI} dpi")

    (out / "labels.json").write_text(json.dumps(labels, indent=2))
    print(f"wrote {out / 'labels.json'}")


if __name__ == "__main__":
    main()
