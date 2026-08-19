#!/usr/bin/env python3
"""co-closeout.py — the operator's closeout review surface, rendered
from the record.

The seat hand-built a closeout page on 2026-08-07 (kept beside this
script as co-closeout-reference.html — the design reference). This is
that page made deterministic: it reads the stores that already exist
and renders one self-contained HTML. No daemon, no new stores, no
knobs. Fieldglass is the pattern.

    scripts/co-closeout.py [--since <duration|date>] [--open]
    scripts/co-closeout.py --self-test

Reads (all read-only):
  * $CO_DIRECTIVE_LOG or ~/.sovereign/comaintainer/directives.jsonl
  * verdicts.jsonl, beside the directives log
  * <main checkout>/.sovereign/features/*/order.md frontmatter

Writes exactly one file: closeout.html, beside the directives log
(so a test pointing CO_DIRECTIVE_LOG at a temp dir contaminates
nothing — the same reason the logger honors that override).

PENDING FIRST is the point. The operator's real closeout burden is
the small "drip" decisions, not only the formal reviews (operator
directive 2026-08-07). The seat's protocol is to log each drip
decision as its own pending row (kind=decision) carrying context +
recommendation + a "Default:" line; this renderer's job is to make
silence safe by rendering that default visibly, and to report its
ABSENCE when a pending row has none (ARCH §18.2/§18.3 — absence is
reported, never defaulted).

RENDER THE RECORD, NEVER SYNTHESIZE. Every string on the page comes
from a store. Drafts are rendered verbatim. If a draft reads badly,
that is feedback for the seat's drafting, not a transformation for
this script to invent.
"""

from __future__ import annotations

import argparse
import datetime as dt
import html
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

# --- the record's shape ---------------------------------------------------
#
# Three record shapes share directives.jsonl, and this join is the SAME
# join scripts/co-directive-log.sh --stats performs (field semantics
# identical, deliberately — one decider, one name):
#
#   legacy one-shot : no `status` field  -> resolved at write time
#   status=pending  : carries id + draft -> awaiting the operator
#   status=resolved : carries id + final -> joins to its pending on id
#
# A second parser is unavoidable (the logger's join lives inside a bash
# heredoc, not an importable module) so it is kept literally equivalent:
# open pending = pending ids minus resolved ids, and `edited` is read
# off the resolved record exactly as the logger wrote it.

# `kind` is an OPEN SET (ARCH §4). Known kinds get this ordering;
# anything else still renders, sorted, after them — never dropped.
KNOWN_KIND_ORDER = ["decision", "order", "steer", "review", "briefing"]

# A pending draft may carry its own safe default. Matched loosely
# because the seat writes prose, not a schema: the marker appears both
# on a line of its own ("Default: seat prunes them") and as a trailing
# clause inside a paragraph ("...frozen semantics per note 0ab26b1d.
# Default: dispatches on your approve." — live directive d1df2275).
# An own-line-only pattern reported that real row as having NO default,
# which is precisely the false negative this section exists to prevent.
DEFAULT_LINE = re.compile(
    r"(?:\*\*)?\bdefault(?:\s+if\s+unresolved)?(?:\*\*)?\s*:\s*(\S[^\n]*)",
    re.IGNORECASE,
)


class Malformed:
    """A line that would not parse. Reported in the footer with its
    line number — never silently skipped (ARCH §18.3)."""

    def __init__(self, path: Path, lineno: int, err: str, raw: str):
        self.path, self.lineno, self.err = path, lineno, err
        self.raw = raw[:160]


def read_jsonl(path: Path, malformed: list) -> list:
    rows = []
    if not path.exists():
        return rows
    with path.open(encoding="utf-8", errors="replace") as fh:
        for lineno, line in enumerate(fh, 1):
            if not line.strip():
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError as exc:
                malformed.append(Malformed(path, lineno, str(exc), line.strip()))
    return rows


# --- time -----------------------------------------------------------------


def parse_ts(value):
    if not isinstance(value, str):
        return None
    try:
        stamp = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if stamp.tzinfo is None:
        stamp = stamp.replace(tzinfo=dt.timezone.utc)
    return stamp


def parse_since(spec: str, now: dt.datetime) -> dt.datetime:
    """`3d` / `12h` / `90m` / `45s`, or a date / ISO datetime."""
    m = re.fullmatch(r"(\d+(?:\.\d+)?)\s*([smhdw])", spec.strip(), re.IGNORECASE)
    if m:
        mult = {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}[m.group(2).lower()]
        return now - dt.timedelta(seconds=float(m.group(1)) * mult)
    stamp = parse_ts(spec.strip())
    if stamp is None:
        raise SystemExit(
            f"co-closeout: --since {spec!r} is neither a duration "
            f"(3d/12h/90m) nor a date (2026-08-07 / ISO datetime)"
        )
    return stamp


def local_midnight(now: dt.datetime) -> dt.datetime:
    local = now.astimezone()
    return local.replace(hour=0, minute=0, second=0, microsecond=0)


def age(stamp, now: dt.datetime) -> str:
    if stamp is None:
        return "unknown"
    secs = (now - stamp).total_seconds()
    if secs < 0:
        return "in the future"
    if secs < 90:
        return f"{secs:.0f}s"
    if secs < 5400:
        return f"{secs / 60:.0f}m"
    if secs < 172800:
        return f"{secs / 3600:.0f}h"
    return f"{secs / 86400:.0f}d"


def local_str(stamp) -> str:
    return "unknown" if stamp is None else stamp.astimezone().strftime("%Y-%m-%d %H:%M")


# --- the stores -----------------------------------------------------------


def directive_log_path() -> Path:
    # Same override the logger honors, for the same reason: tests must
    # never contaminate the real edit-rate metric.
    env = os.environ.get("CO_DIRECTIVE_LOG")
    if env:
        return Path(env).expanduser()
    return Path.home() / ".sovereign" / "comaintainer" / "directives.jsonl"


def orders_dir(script_path: Path):
    """Orders are gitignored per-host state under the MAIN checkout; a
    linked worktree has no .sovereign/features of its own. git's
    common-dir points at the main checkout's .git from anywhere, so a
    worker running this from a worktree still sees the real orders."""
    here = script_path.resolve().parent.parent
    try:
        out = subprocess.run(
            ["git", "-C", str(here), "rev-parse", "--git-common-dir"],
            capture_output=True, text=True, timeout=10,
        )
        if out.returncode == 0 and out.stdout.strip():
            common = Path(out.stdout.strip())
            if not common.is_absolute():
                common = here / common
            candidate = common.resolve().parent / ".sovereign" / "features"
            if candidate.is_dir():
                return candidate
    except (OSError, subprocess.SubprocessError):
        pass
    return here / ".sovereign" / "features"


FRONTMATTER_KEY = re.compile(r"^([a-z_]+):\s*(.*)$")


def read_orders(features: Path):
    """Frontmatter of every .sovereign/features/*/order.md, plus the
    `# Order: <title>` line. Written by scripts/co-order.sh."""
    orders = []
    if not features.is_dir():
        return orders
    for path in sorted(features.glob("*/order.md")):
        meta, title, in_front, seen_open = {}, None, False, False
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            orders.append({"id": path.parent.name, "title": f"(unreadable: {exc})",
                           "status": "?", "approved": "?", "drafted": None,
                           "path": path})
            continue
        for line in text.splitlines():
            if line.strip() == "---":
                if not seen_open:
                    seen_open, in_front = True, True
                    continue
                in_front = False
                continue
            if in_front:
                m = FRONTMATTER_KEY.match(line)
                if m:
                    meta[m.group(1)] = m.group(2).strip()
            elif line.startswith("# Order:"):
                title = line[len("# Order:"):].strip()
                break
        orders.append({
            "id": meta.get("id", path.parent.name),
            "title": title or "(no title line)",
            "status": meta.get("status", "(no status)"),
            "approved": meta.get("approved", "(none)"),
            "drafted": meta.get("drafted"),
            "path": path,
        })
    return orders


def join_directives(rows):
    """The logger's join, field-for-field. Returns
    (open_pending, resolved_pairs) where a pair is
    (pending_or_None, resolved_or_legacy)."""
    pending = {}
    for r in rows:
        if r.get("status") == "pending" and "id" in r:
            pending[r["id"]] = r
    resolved = [r for r in rows if r.get("status") == "resolved"]
    resolved_ids = {r.get("id") for r in resolved}
    open_ids = [i for i in pending if i not in resolved_ids]
    open_pending = [pending[i] for i in open_ids]

    pairs = []
    for r in resolved:
        pairs.append((pending.get(r.get("id")), r))
    for r in rows:  # legacy one-shot: draft and final on one record
        if "status" not in r:
            pairs.append((None, r))
    return open_pending, pairs


def kind_sort_key(kind: str):
    kind = kind or "(no kind)"
    if kind in KNOWN_KIND_ORDER:
        return (0, KNOWN_KIND_ORDER.index(kind), kind)
    return (1, 0, kind)  # open set: unknown kinds render, sorted, after


# --- rendering ------------------------------------------------------------

E = html.escape

CSS = """
:root{
  --ground:#FAF9F5; --panel:#FFFFFF; --ink:#20241F; --meta:#6E7369; --rule:#E5E3D9;
  --ok:#3E6B50; --ok-soft:#EAF1EC; --pend:#A8721F; --pend-soft:#F7EFDF;
  --code-bg:#F1F0E9; --shadow:0 1px 3px rgba(32,36,31,.06);
}
@media (prefers-color-scheme: dark){:root{
  --ground:#191B18; --panel:#20231F; --ink:#E7E6DF; --meta:#9AA096; --rule:#31352E;
  --ok:#7FB393; --ok-soft:#24322A; --pend:#D9A24C; --pend-soft:#332B1D;
  --code-bg:#262922; --shadow:none;
}}
:root[data-theme="dark"]{
  --ground:#191B18; --panel:#20231F; --ink:#E7E6DF; --meta:#9AA096; --rule:#31352E;
  --ok:#7FB393; --ok-soft:#24322A; --pend:#D9A24C; --pend-soft:#332B1D;
  --code-bg:#262922; --shadow:none;
}
:root[data-theme="light"]{
  --ground:#FAF9F5; --panel:#FFFFFF; --ink:#20241F; --meta:#6E7369; --rule:#E5E3D9;
  --ok:#3E6B50; --ok-soft:#EAF1EC; --pend:#A8721F; --pend-soft:#F7EFDF;
  --code-bg:#F1F0E9; --shadow:0 1px 3px rgba(32,36,31,.06);
}
*{box-sizing:border-box}
body{margin:0;background:var(--ground);color:var(--ink);
  font:15px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
  padding:40px 20px 80px}
main{max-width:860px;margin:0 auto;display:flex;flex-direction:column;gap:36px}
h1{font-size:26px;margin:0;letter-spacing:-.01em;text-wrap:balance}
h2{font-size:13px;margin:0;text-transform:uppercase;letter-spacing:.09em;color:var(--meta);font-weight:600}
h3{font-size:12px;margin:0;text-transform:uppercase;letter-spacing:.07em;color:var(--pend);font-weight:600}
p{margin:0}
.sub{color:var(--meta);margin-top:6px}
.chips{display:flex;gap:8px;flex-wrap:wrap;margin-top:14px}
.chip{font-size:12.5px;padding:3px 10px;border:1px solid var(--rule);border-radius:99px;color:var(--meta)}
.chip b{color:var(--ink);font-weight:600}
section{display:flex;flex-direction:column;gap:14px}
.card{background:var(--panel);border:1px solid var(--rule);border-radius:10px;box-shadow:var(--shadow);overflow:hidden}
.card>header{display:flex;align-items:center;gap:10px;padding:14px 18px;border-bottom:1px solid var(--rule);flex-wrap:wrap}
.ref{font:600 13px/1 ui-monospace,SFMono-Regular,Menlo,monospace;background:var(--code-bg);
  padding:5px 9px;border-radius:6px}
.kind{font-size:11.5px;text-transform:uppercase;letter-spacing:.07em;color:var(--meta)}
.pill{margin-left:auto;font-size:12px;font-weight:600;padding:4px 11px;border-radius:99px;
  background:var(--pend-soft);color:var(--pend)}
.card .body{padding:14px 18px;display:flex;flex-direction:column;gap:12px}
.lbl{font-size:11.5px;text-transform:uppercase;letter-spacing:.07em;color:var(--meta);margin-bottom:4px}
ul{margin:0;padding-left:18px;display:flex;flex-direction:column;gap:5px}
code{font:12.5px ui-monospace,SFMono-Regular,Menlo,monospace;background:var(--code-bg);padding:1.5px 5px;border-radius:4px}
.headline{border-left:3px solid var(--pend);background:var(--pend-soft);padding:10px 14px;border-radius:0 8px 8px 0;font-size:14px}
.nodefault{border-left:3px solid var(--rule);padding:10px 14px;font-size:13.5px;color:var(--meta)}
pre{background:var(--code-bg);border:1px solid var(--rule);border-radius:8px;padding:12px 14px;overflow-x:auto;
  font:12.5px/1.7 ui-monospace,SFMono-Regular,Menlo,monospace;margin:0;white-space:pre-wrap}
.tablewrap{overflow-x:auto;background:var(--panel);border:1px solid var(--rule);border-radius:10px}
table{border-collapse:collapse;width:100%;font-size:13.5px;min-width:640px}
th{font-size:11px;text-transform:uppercase;letter-spacing:.07em;color:var(--meta);font-weight:600;text-align:left}
th,td{padding:8px 14px;border-bottom:1px solid var(--rule);vertical-align:top}
tr:last-child td{border-bottom:none}
td.t,td.r{font:12.5px ui-monospace,SFMono-Regular,Menlo,monospace;white-space:nowrap;font-variant-numeric:tabular-nums}
.res{color:var(--ok);font-weight:600}
.edited{color:var(--pend);font-weight:600}
details summary{cursor:pointer;color:var(--meta)}
details[open] summary{margin-bottom:6px}
.empty{background:var(--panel);border:1px dashed var(--rule);border-radius:10px;padding:16px 18px;color:var(--meta)}
.foot{color:var(--meta);font-size:12.5px;border-top:1px solid var(--rule);padding-top:14px;line-height:1.7}
.bad{color:var(--pend)}
"""


def empty(msg: str) -> str:
    return f'<div class="empty">{E(msg)}</div>'


def excerpt(text: str, limit: int = 160) -> str:
    """Collapsed view for a table cell. The full text is always one
    click away in the same <details> — nothing is truncated away."""
    flat = " ".join((text or "").split())
    return flat if len(flat) <= limit else flat[:limit] + "…"


def details_block(label: str, text: str) -> str:
    if not text:
        return f'<div class="lbl">{E(label)}</div><div class="bad">(absent from the record)</div>'
    return (
        f"<details><summary>{E(label)}: {E(excerpt(text))}</summary>"
        f"<pre>{E(text)}</pre></details>"
    )


def find_default(draft: str):
    """The LAST default marker in the draft wins — a draft that restates
    its fallback is stating the final word, and taking the first would
    show a superseded one. Returns None when the draft has no marker at
    all; that absence is rendered, not defaulted."""
    found = None
    for m in DEFAULT_LINE.finditer(draft or ""):
        found = m.group(1).strip().rstrip("*").strip()
    return found or None


def render_ledger(open_pending, now) -> str:
    if not open_pending:
        return (
            "<section><h2>The decision ledger — every pending call, however small</h2>"
            + empty("No pending directives. The ledger is empty because the "
                    "record says so, not because nothing was read.")
            + "</section>"
        )
    by_kind = {}
    for row in open_pending:
        by_kind.setdefault(row.get("kind") or "(no kind)", []).append(row)

    parts = [
        "<section><h2>The decision ledger — every pending call, however small</h2>",
        f'<p class="sub">{len(open_pending)} pending across '
        f"{len(by_kind)} kind(s). Drafts are verbatim from the log.</p>",
    ]
    for kind in sorted(by_kind, key=kind_sort_key):
        rows = sorted(by_kind[kind], key=lambda r: parse_ts(r.get("ts")) or now)
        known = "" if kind in KNOWN_KIND_ORDER else ' <span class="kind">(kind not in the known set — rendered anyway)</span>'
        parts.append(f"<h3>{E(kind)} · {len(rows)}{known}</h3>")
        for row in rows:
            stamp = parse_ts(row.get("ts"))
            draft = row.get("draft") or ""
            default = find_default(draft)
            worker = row.get("worker")
            cits = [c for c in (row.get("citations") or []) if c]
            body = [f"<pre>{E(draft) if draft else '(no draft on the record)'}</pre>"]
            if default:
                body.append(
                    '<div class="headline"><b>If you say nothing:</b> '
                    f"{E(default)}</div>"
                )
            else:
                body.append(
                    '<div class="nodefault">No <code>Default:</code> line in the '
                    "draft — silence leaves this open, with no recorded fallback.</div>"
                )
            meta_bits = [f"logged {E(local_str(stamp))}"]
            if worker:
                meta_bits.append(f"worker <code>{E(str(worker))}</code>")
            if cits:
                meta_bits.append("cites " + ", ".join(f"<code>{E(c)}</code>" for c in cits))
            body.append('<p class="sub">' + " · ".join(meta_bits) + "</p>")
            parts.append(
                '<div class="card"><header>'
                f'<span class="ref">{E(str(row.get("id", "(no id)")))}</span>'
                f'<span class="kind">{E(kind)}</span>'
                f'<span class="pill">pending {E(age(stamp, now))}</span>'
                "</header>"
                f'<div class="body">{"".join(body)}</div></div>'
            )
    parts.append("</section>")
    return "".join(parts)


def render_resolved(pairs, since, now, since_label: str) -> str:
    in_window = []
    for pending, resolved in pairs:
        stamp = parse_ts(resolved.get("ts"))
        if stamp is not None and stamp >= since:
            in_window.append((pending, resolved, stamp))
    in_window.sort(key=lambda t: t[2])

    head = (
        "<section><h2>Resolved in window — the (draft → final) record</h2>"
        f'<p class="sub">Window: since {E(since_label)}.</p>'
    )
    if not in_window:
        return head + empty(
            f"No directives resolved since {since_label}. "
            f"{len(pairs)} resolved record(s) exist outside the window."
        ) + "</section>"

    rows = [
        "<div class=\"tablewrap\"><table><tr><th>ref</th><th>kind</th><th>when</th>"
        "<th>latency</th><th>draft → final</th><th>resolution</th></tr>"
    ]
    for pending, resolved, stamp in in_window:
        ref = resolved.get("id") or "one-shot"
        kind = resolved.get("kind") or (pending or {}).get("kind") or "(no kind)"
        draft = (pending or resolved).get("draft") or ""
        final = resolved.get("final") or ""
        edited = resolved.get("edited")
        klass, verdict = ("res", "verbatim")
        if edited:
            klass, verdict = ("res edited", "edited")
        elif edited is None:
            klass, verdict = ("bad", "not recorded")
        ec = resolved.get("edit_class")
        if ec and edited:
            verdict += f" ({ec})"
        # Latency is computable only for a joined pending→resolved pair;
        # a legacy one-shot was resolved at write time and has none.
        lat = "—"
        if pending is not None:
            p = parse_ts(pending.get("ts"))
            if p is not None:
                lat = age(p, stamp)
        elif "status" not in resolved:
            lat = "at write"
        rows.append(
            f'<tr><td class="r">{E(str(ref))}</td><td>{E(str(kind))}</td>'
            f'<td class="t">{E(local_str(stamp))}</td><td class="t">{E(lat)}</td>'
            f"<td>{details_block('draft', draft)}{details_block('final', final)}</td>"
            f'<td class="{klass}">{E(verdict)}</td></tr>'
        )
    rows.append("</table></div></section>")
    return head + "".join(rows)


def render_orders(orders, features: Path, now) -> str:
    head = "<section><h2>Open orders</h2>"
    if not orders:
        return head + empty(
            f"No order files found under {features}. "
            "(Orders are gitignored per-host state — an empty features "
            "directory is a valid state, not a failed read.)"
        ) + "</section>"
    open_orders = [o for o in orders if o["status"] == "open"]
    if not open_orders:
        return head + empty(
            f"No open orders. {len(orders)} order file(s) scanned under "
            f"{features}; every one is closed."
        ) + "</section>"
    rows = [
        f'<p class="sub">{len(open_orders)} open of {len(orders)} scanned '
        f"under <code>{E(str(features))}</code>.</p>",
        '<div class="tablewrap"><table><tr><th>id</th><th>title</th>'
        "<th>status</th><th>approved</th><th>age</th></tr>",
    ]
    for o in sorted(open_orders, key=lambda o: o["drafted"] or ""):
        drafted = parse_ts(o["drafted"]) if o["drafted"] else None
        rows.append(
            f'<tr><td class="r">{E(o["id"])}</td><td>{E(o["title"])}</td>'
            f'<td>{E(o["status"])}</td><td class="t">{E(o["approved"])}</td>'
            f'<td class="t">{E(age(drafted, now) if drafted else "no drafted date")}</td></tr>'
        )
    rows.append("</table></div></section>")
    return head + "".join(rows)


def render_verdicts(verdicts, path: Path, now, limit: int = 10) -> str:
    head = "<section><h2>Recent verdicts</h2>"
    if not verdicts:
        return head + empty(
            f"No verdicts in {path}. (co-review.sh writes them; an "
            "empty file means the sweep has not run, not that it passed.)"
        ) + "</section>"
    ordered = sorted(verdicts, key=lambda v: parse_ts(v.get("ts")) or dt.datetime.min.replace(tzinfo=dt.timezone.utc))
    shown = ordered[-limit:][::-1]
    rows = [
        f'<p class="sub">Last {len(shown)} of {len(verdicts)} in '
        f"<code>{E(str(path))}</code>.</p>",
        '<div class="tablewrap"><table><tr><th>ref</th><th>when</th>'
        "<th>verdict</th><th>basis</th></tr>",
    ]
    for v in shown:
        ref = str(v.get("ref") or "(no ref)")
        basis = v.get("basis") or []
        if isinstance(basis, str):
            basis = [basis]
        # Three states, not two (§18.2). Before G1 this page rendered every
        # anchor identically, which meant a citation nobody had resolved
        # looked exactly like one that had — the page was the last place
        # an invented anchor could have been caught and it presented them
        # as checked. Rows written before G1 carry no `basis_checked` key
        # at all; those are UNKNOWN and must not be back-dated into
        # "verified" by a truthy default.
        checked = v.get("basis_checked")
        unresolved = set(v.get("basis_unresolved") or [])
        parts = []
        for b in basis:
            b = str(b)
            if b in unresolved:
                parts.append(f'<code class="bad">{E(b)}</code> '
                             '<span class="bad">(does not resolve)</span>')
            elif checked is True:
                parts.append(f'<code class="res">{E(b)}</code>')
            else:
                parts.append(f"<code>{E(b)}</code>")
        basis_html = ", ".join(parts) or \
            '<span class="bad">(no basis recorded)</span>'
        if basis and checked is not True:
            basis_html += ' <span class="sub">(not verified)</span>'
        verdict = str(v.get("verdict") or "(no verdict)")
        klass = "res" if verdict == "approve" else "bad"
        rows.append(
            f'<tr><td class="r">{E(ref[:12])}</td>'
            f'<td class="t">{E(local_str(parse_ts(v.get("ts"))))}</td>'
            f'<td class="{klass}">{E(verdict)}</td><td>{basis_html}</td></tr>'
        )
    rows.append("</table></div></section>")
    return head + "".join(rows)


def render_footer(sources, malformed, generated_at) -> str:
    bits = ["Sources: " + " · ".join(f"<code>{E(str(s))}</code>" for s in sources)]
    if malformed:
        items = "".join(
            f"<li><code>{E(str(m.path))}</code> line <b>{m.lineno}</b>: "
            f"{E(m.err)} — raw: <code>{E(m.raw)}</code></li>"
            for m in malformed
        )
        bits.append(
            f'<span class="bad"><b>{len(malformed)} malformed line(s) could '
            f"not be parsed</b> — reported, not skipped:</span><ul>{items}</ul>"
        )
    else:
        bits.append("Every line of every source parsed; no malformed records.")
    bits.append(
        f"Rendered {E(generated_at)} by <code>scripts/co-closeout.py</code>. "
        "Every string above is drawn from a store — nothing on this page "
        "was written by the renderer."
    )
    return '<p class="foot">' + "<br>".join(bits) + "</p>"


def build_page(log_path: Path, now: dt.datetime, since: dt.datetime,
               since_label: str, script_path: Path) -> str:
    malformed: list = []
    directives = read_jsonl(log_path, malformed)
    verdicts_path = log_path.parent / "verdicts.jsonl"
    verdicts = read_jsonl(verdicts_path, malformed)
    features = orders_dir(script_path)
    orders = read_orders(features)

    open_pending, pairs = join_directives(directives)
    resolved_in_window = sum(
        1 for _, r in pairs
        if (parse_ts(r.get("ts")) or dt.datetime.min.replace(tzinfo=dt.timezone.utc)) >= since
    )
    open_orders = sum(1 for o in orders if o["status"] == "open")

    if not directives:
        headline = (
            f"<p class=\"sub\">No directives in window — "
            f"<code>{E(str(log_path))}</code> holds no records at all. "
            "Reported, not rendered blank.</p>"
        )
    else:
        headline = (
            f'<p class="sub">{E(local_str(now))} · window since '
            f"{E(since_label)} · drawn from "
            f"<code>{E(str(log_path))}</code></p>"
        )

    chips = "".join(
        f'<span class="chip"><b>{n}</b> {label}</span>'
        for n, label in [
            (len(open_pending), "pending your call"),
            (resolved_in_window, "resolved in window"),
            (open_orders, "open orders"),
            (len(verdicts), "verdicts on file"),
        ]
    )

    body = "".join([
        "<div><h1>Closeout</h1>", headline,
        f'<div class="chips">{chips}</div></div>',
        render_ledger(open_pending, now),
        render_resolved(pairs, since, now, since_label),
        render_orders(orders, features, now),
        render_verdicts(verdicts, verdicts_path, now),
        render_footer([log_path, verdicts_path, features], malformed, local_str(now)),
    ])
    return (
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">"
        "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">"
        f"<title>Closeout — {E(now.astimezone().strftime('%Y-%m-%d'))}</title>"
        f"<style>{CSS}</style></head><body><main>{body}</main></body></html>\n"
    )


def render(log_path: Path, since_spec, script_path: Path) -> Path:
    now = dt.datetime.now(dt.timezone.utc)
    if since_spec:
        since, label = parse_since(since_spec, now), since_spec
    else:
        since = local_midnight(now)
        label = "local midnight (" + since.strftime("%Y-%m-%d %H:%M") + ")"
    out = log_path.parent / "closeout.html"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(build_page(log_path, now, since, label, script_path),
                   encoding="utf-8")
    return out


# --- the lane -------------------------------------------------------------
#
# --self-test IS the lane. Three checks, each watched in BOTH directions:
# the assertion that must hold on one input is also asserted NOT to hold
# on its opposite, so a check that can never fail is visible as such
# (ARCH §18.1: a check with no failing input you can name is not a check).

FIXTURE = [
    # a pending drip decision, with a recorded default
    {"id": "aaaa1111", "ts": "{t0}", "status": "pending", "worker": None,
     "kind": "decision",
     "draft": "Prune the two freed worktrees after the merge train.\n"
              "Recommendation: yes, seat does it as a chore.\n"
              "Default: seat prunes them; skunkworks stays.",
     "citations": ["ARCH §14"], "charter_sha256": None},
    # a pending row whose default is an INLINE trailing clause, not its
    # own line — the shape the seat actually writes (observed on the
    # live log 2026-08-07, directive d1df2275).
    {"id": "eeee5555", "ts": "{t0}", "status": "pending", "worker": "w5",
     "kind": "order",
     "draft": "Order enrichment-blind-arms: close the three blind arms. "
              "New worktree; opus/medium. Default: dispatches on your approve.",
     "citations": [], "charter_sha256": None},
    # a pending row of an UNKNOWN kind, and with NO default line
    {"id": "bbbb2222", "ts": "{t0}", "status": "pending", "worker": "w2",
     "kind": "smoke-signal",
     "draft": "An unknown kind must still render.",
     "citations": [], "charter_sha256": None},
    # a pending → resolved pair the operator EDITED
    {"id": "cccc3333", "ts": "{t1}", "status": "pending", "worker": "w3",
     "kind": "order", "draft": "DRAFTED-TEXT-ALPHA",
     "citations": [], "charter_sha256": None},
    {"id": "cccc3333", "ts": "{t2}", "status": "resolved", "kind": "order",
     "final": "FINAL-TEXT-BETA", "edited": True, "edit_class": "content"},
    # a legacy one-shot, approved VERBATIM
    {"ts": "{t2}", "worker": "w4", "kind": "review",
     "draft": "ONESHOT-TEXT-GAMMA", "final": "ONESHOT-TEXT-GAMMA",
     "edited": False, "edit_class": "none", "citations": [],
     "charter_sha256": None},
]


def _fixture_lines(now: dt.datetime) -> list:
    subs = {
        "t0": (now - dt.timedelta(hours=3)).isoformat(),
        "t1": (now - dt.timedelta(hours=2)).isoformat(),
        "t2": (now - dt.timedelta(hours=1)).isoformat(),
    }
    out = []
    for rec in FIXTURE:
        rec = {k: (v.format(**subs) if k == "ts" else v) for k, v in rec.items()}
        out.append(json.dumps(rec, ensure_ascii=False))
    return out


def self_test(script_path: Path) -> int:
    now = dt.datetime.now(dt.timezone.utc)
    failures = []

    def check(name: str, ok: bool, detail: str = ""):
        print(f"  {'PASS' if ok else 'FAIL'}  {name}" + (f" — {detail}" if detail else ""))
        if not ok:
            failures.append(name)

    with tempfile.TemporaryDirectory(prefix="co-closeout-selftest-") as tmp:
        tmp = Path(tmp)

        # --- direction 1: a populated fixture log ------------------------
        full = tmp / "full" / "directives.jsonl"
        full.parent.mkdir(parents=True)
        full.write_text("\n".join(_fixture_lines(now)) + "\n", encoding="utf-8")
        page = render(full, "6h", script_path).read_text(encoding="utf-8")

        print("check 1 — fixture log renders the record")
        check("pending row present (ref aaaa1111)", "aaaa1111" in page)
        check("draft rendered VERBATIM",
              "Prune the two freed worktrees after the merge train." in page)
        check("recorded Default surfaced as the silence-is-safe line",
              "If you say nothing:" in page
              and "seat prunes them; skunkworks stays." in page)
        # Scoped to the one card, so the verbatim <pre> (which also
        # contains the sentence) cannot make this check pass by itself.
        inline_card = next(c for c in page.split('<div class="card">')
                           if "eeee5555" in c)
        check("INLINE trailing default surfaced too (the shape the seat writes)",
              "If you say nothing:" in inline_card
              and "silence leaves this open" not in inline_card)
        check("pending row WITHOUT a default says so",
              "silence leaves this open" in page)
        check("unknown kind still rendered (open set)",
              "smoke-signal" in page and "not in the known set" in page)
        check("resolved pair: edited distinguished",
              ">edited (content)<" in page)
        check("resolved one-shot: verbatim distinguished", ">verbatim<" in page)
        check("both sides of the pair present",
              "DRAFTED-TEXT-ALPHA" in page and "FINAL-TEXT-BETA" in page)
        # The fixture pair is pending t1 → resolved t2, exactly 1h apart;
        # age() reports anything under 90 min in minutes, hence "60m".
        check("resolution latency computed for the joined pair",
              '<td class="t">60m</td>' in page)
        check("legacy one-shot reports 'at write', not a fabricated latency",
              '<td class="t">at write</td>' in page)
        check("NEGATIVE: does not claim an empty window",
              "No directives in window" not in page)

        # --- direction 2: an empty log -----------------------------------
        blank = tmp / "blank" / "directives.jsonl"
        blank.parent.mkdir(parents=True)
        blank.write_text("", encoding="utf-8")
        page2 = render(blank, "6h", script_path).read_text(encoding="utf-8")

        print("check 2 — empty log renders an honest empty state")
        check("says 'No directives in window'", "No directives in window" in page2)
        check("ledger reports the absence rather than going blank",
              "The ledger is empty because the record says so" in page2)
        check("NEGATIVE: none of the fixture leaks in",
              "aaaa1111" not in page2 and "DRAFTED-TEXT-ALPHA" not in page2)

        # --- direction 3: a malformed line -------------------------------
        bad = tmp / "bad" / "directives.jsonl"
        bad.parent.mkdir(parents=True)
        lines = _fixture_lines(now)
        lines.insert(1, '{"id": "dddd4444", "ts": broken-json,,,')
        bad.write_text("\n".join(lines) + "\n", encoding="utf-8")
        page3 = render(bad, "6h", script_path).read_text(encoding="utf-8")

        print("check 3 — a malformed line is reported, never skipped")
        check("footer names the malformed count",
              "malformed line(s) could not be parsed" in page3)
        check("footer names the LINE NUMBER (2)", "line <b>2</b>" in page3)
        check("the raw head of the bad line is shown",
              "broken-json" in page3)
        check("the good records still rendered", "aaaa1111" in page3)
        check("NEGATIVE: the clean render claims no malformed lines",
              "no malformed records" in page and
              "malformed line(s) could not be parsed" not in page)

        # --- check 4: G1 — a cited anchor that does not resolve ----------
        # Both halves of G1 in one place, because splitting them is how
        # the hole existed in the first place: the gate can demote, and
        # the page can still render the anchor as if it were checked.
        #
        # The GATE half runs against an injected resolver, not the real
        # one. `BasisResolver` reads the repo, the ledger and
        # ~/.sovereign/notes.db; on a host without them every anchor is
        # unresolvable and this check would pass for the wrong reason.
        # What is under test here is the gate's decision, and the
        # resolver itself is already exercised by the gym scorer.
        print("check 4 — G1: an unresolvable citation demotes, and says so")
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent
                               / "gym" / "comaintainer"))
        try:
            from score import basis_gate
        except Exception as e:  # noqa: BLE001
            check("score.basis_gate is importable", False, f"{type(e).__name__}: {e}")
            basis_gate = None

        if basis_gate is not None:
            class _Stub:
                REAL = {"ARCH §18.3", "commit abc1234"}

                def exists(self, anchor):
                    return anchor in self.REAL

            fabricated = basis_gate(
                {"verdict": "approve", "basis": ["ARCH §18.3", "ARCH §99.9",
                                                 "note deadbeef"]}, _Stub())
            # `.get`, not `[...]`: when the gate stops demoting, the key
            # is absent, and this lane must report FAIL rather than die
            # with a KeyError that reads like a broken test.
            check("a fabricated anchor demotes the verdict",
                  fabricated.get("verdict") == "could-not-judge",
                  f"got {fabricated.get('verdict')!r}")
            check("the engine's own verdict is preserved, not erased",
                  fabricated.get("proposed_verdict") == "approve")
            check("the demotion names WHICH anchors failed",
                  fabricated["basis_unresolved"] == ["ARCH §99.9", "note deadbeef"],
                  str(fabricated["basis_unresolved"]))
            check("the resolvable anchor is not blamed",
                  "ARCH §18.3" not in fabricated["basis_unresolved"])

            clean = basis_gate(
                {"verdict": "approve", "basis": ["ARCH §18.3", "commit abc1234"]},
                _Stub())
            check("NEGATIVE: every anchor resolving does NOT demote",
                  "verdict" not in clean and clean["basis_checked"] is True)

            empty = basis_gate({"verdict": "approve", "basis": []}, _Stub())
            check("NEGATIVE: an empty basis demotes nothing",
                  "verdict" not in empty and empty["basis_unresolved"] == [])

            # The RENDER half: three states must look like three states.
            now4 = dt.datetime.now(dt.timezone.utc)
            vpath = tmp / "verdicts.jsonl"
            vpage = render_verdicts([
                {"ts": now4.isoformat(), "ref": "1111111", **fabricated},
                {"ts": now4.isoformat(), "ref": "2222222", "verdict": "approve",
                 **clean},
                # A row written BEFORE G1: no basis_checked key at all.
                {"ts": now4.isoformat(), "ref": "3333333", "verdict": "approve",
                 "basis": ["ARCH §18.3"]},
            ], vpath, now4)
            check("the page marks the anchor that does not resolve",
                  "does not resolve" in vpage)
            check("a pre-G1 row is shown as NOT verified, not as checked",
                  "(not verified)" in vpage)
            check("NEGATIVE: a fully-resolved row carries neither warning",
                  vpage.count("does not resolve") == 2  # one per bad anchor
                  and vpage.count("(not verified)") == 1)

    print()
    if failures:
        print(f"self-test FAILED — {len(failures)} check(s): " + "; ".join(failures))
        return 1
    print("self-test PASSED — 4 checks, both directions each.")
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        prog="co-closeout.py",
        description="Render the operator's closeout page from the comaintainer record.")
    ap.add_argument("--since", metavar="DUR|DATE",
                    help="scope the resolved table (3d / 12h / 90m / 2026-08-07). "
                         "Default: local midnight.")
    ap.add_argument("--open", action="store_true", dest="open_it",
                    help="open the rendered page in a browser")
    ap.add_argument("--self-test", action="store_true",
                    help="run the render lane (fixture / empty / malformed) and exit")
    args = ap.parse_args(argv)

    script_path = Path(__file__)
    if args.self_test:
        return self_test(script_path)

    log = directive_log_path()
    if not log.exists():
        # §18.3: absence is reported, never rendered as an empty success.
        print(f"co-closeout: no directives log at {log} — nothing to render. "
              "(Set CO_DIRECTIVE_LOG if it lives elsewhere.)", file=sys.stderr)
        return 2
    out = render(log, args.since, script_path)
    print(out)
    if args.open_it:
        import webbrowser
        webbrowser.open(out.as_uri())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
