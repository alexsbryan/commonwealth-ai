#!/usr/bin/env python3
"""co-lineage.py — campaign flight rules: screen-sized limits, machine-stamped rows.

Substrate replaced 2026-08-17 (order campaign-telemetry, operator decision).
The predecessor read `quality/initiative-bars.toml` — 3,564 lines, 74 bars,
163 hand-written prose transitions, no closure loop. Hand-written transitions
were the failure: on the day the replacement plan was written, a prose row
asserted `F2-honesty-in-answer-path -> met` crediting a guard that exists only
on an unmerged branch (`authority_guard.rs` absent at HEAD 51e2fbaa). A
measurement row stamping ref + ref_source + dirty catches that mechanically.

The substrate now:
  quality/campaigns/<id>.toml    flight rules — ONE screen, <=9 bars (load
                                 error over), numeric thresholds, executable
                                 `instrument`. NO transition rows, ever.
  ~/.sovereign/comaintainer/bar-measurements.jsonl
                                 append-only machine-stamped rows; verdict =
                                 newest row; history = trend. never-attempted
                                 IS the absence of rows (§18.2).
  ~/.sovereign/comaintainer/verdicts.jsonl (kind:"drift")
                                 shadow rows from scripts/co-drift.py.

  scripts/co-lineage.py coverage <campaign>     read path: TOML + two local
                                                jsonl files, <1s, daemon-free,
                                                no git subprocess
  scripts/co-lineage.py measure <campaign>|--all-active [--store PATH]
  scripts/co-lineage.py postmortem <campaign>
  scripts/co-lineage.py list
  scripts/co-lineage.py --self-test

Closure loop: a closed campaign's file moves to quality/campaigns/closed/;
defer/descope a bar = a one-line `status` edit; git history is the ledger.

Instrument contract: last non-empty stdout line is a bare number or
{"value": N, "commit": "<sha>", "artifact": "<path>"}. Exit 0 = value valid,
exit 3 = artifact absent (named), other nonzero = could-not-judge. Instruments
emit VALUES only — the verdict is computed here, by the one decider, from the
thresholds the row snapshots (§10.6, §18.6). Telemetry records, never gates:
`measure` exits 0 on failed verdicts.

Exit codes: 0 rendered/measured; 2 usage / unknown campaign; 3 malformed
declaration (DataError, never defaulted); 5 store unwritable.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import signal
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CAMPAIGNS_DIR = REPO / "quality" / "campaigns"
FEATURES = REPO / ".sovereign" / "features"
CO_DIR = Path.home() / ".sovereign" / "comaintainer"
STORE = CO_DIR / "bar-measurements.jsonl"
DRIFT_STORE = CO_DIR / "verdicts.jsonl"
MEASURE_LOG_DIR = CO_DIR / "measure"

# ---- closed sets: constants in the one reader (§2.1). No per-file
# [vocabulary] section — an unknown value raises DataError, never defaults.
VERDICTS = ("met", "met-floor", "failed", "could-not-judge")
NEVER = "never-attempted"          # structurally the ABSENCE of rows, never written
DIRECTIONS = ("higher_is_better", "lower_is_better", "near_zero")
BAR_STATUS = ("open", "deferred", "descoped")
CAMPAIGN_STATUS = ("active", "closed")
REF_SOURCES = ("head", "artifact", "unattributed")
# reason: "" unless could-not-judge; then exactly one of these shapes.
REASON_RE = re.compile(
    r"^$|^instrument-missing$|^artifact-absent$|^unparseable-output$"
    r"|^exit \d+$|^timeout \d+s$|^import-failure: ")

MAX_BARS = 9                       # over-cap is a LOAD ERROR — altitude is forced
READ_TIER_TIMEOUT_S = 10           # absent timeout_s = read-tier, hard cap
STALE_HOURS = 48
TREND_N = 7                        # §18.5: trend judged over n rows, never one

DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


class DataError(Exception):
    """The declaration said something outside its own closed set."""


# --------------------------------------------------------------------------
# data model
# --------------------------------------------------------------------------


@dataclass
class Bar:
    id: str
    one_line: str
    derives_from: str
    status: str                     # open | deferred | descoped
    floor: float | None = None
    floor_basis: str = ""
    target: float | None = None
    direction: str = "higher_is_better"
    noise_band: float | None = None
    instrument: str = ""
    timeout_s: int | None = None    # presence = run-tier + kill deadline
    kill: str = ""

    @property
    def run_tier(self) -> bool:
        return self.timeout_s is not None


@dataclass
class Campaign:
    id: str
    objective: str
    spec: str
    declared: str
    status: str
    path: Path
    bars: list[Bar] = field(default_factory=list)


@dataclass
class Order:
    id: str
    path: Path
    status: str
    drafted: str
    approved: str
    serves_raw: str | None
    serves_initiative: str | None   # now a campaign id; field name kept for
    serves_bars: list[str]          # importers (co-order.sh) — one vocabulary

    @property
    def attributed(self) -> bool:
        return self.serves_initiative is not None


# --------------------------------------------------------------------------
# loading — missing keys and out-of-set values are DataError, never KeyError
# --------------------------------------------------------------------------


def _req(raw: dict, key: str, where: str):
    if key not in raw:
        raise DataError(f"{where}: required key {key!r} is missing")
    return raw[key]


def _num(value, key: str, where: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise DataError(f"{where}: {key} = {value!r} is not numeric — thresholds "
                        "are numbers the decider can compare, never prose")
    return float(value)


def load_campaign_file(path: Path) -> Campaign:
    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as exc:
        raise DataError(f"{path} is not valid TOML: {exc}")
    where = path.name
    camp = Campaign(
        id=str(_req(raw, "id", where)),
        objective=str(_req(raw, "objective", where)),
        spec=str(_req(raw, "spec", where)),
        declared=str(_req(raw, "declared", where)),
        status=str(_req(raw, "status", where)),
        path=path,
    )
    if not DATE_RE.match(camp.declared):
        raise DataError(f"{where}: declared {camp.declared!r} is not YYYY-MM-DD")
    if camp.status not in CAMPAIGN_STATUS:
        raise DataError(f"{where}: status {camp.status!r} not in {CAMPAIGN_STATUS}")
    bars_raw = raw.get("bar", [])
    if len(bars_raw) > MAX_BARS:
        raise DataError(
            f"{where}: {len(bars_raw)} bars exceeds the cap of {MAX_BARS}. The cap "
            "IS the anti-accretion structure — a campaign that needs more bars "
            "needs a smaller campaign. Escalate; do not widen.")
    seen: set[str] = set()
    for b_raw in bars_raw:
        bwhere = f"{where} bar {b_raw.get('id', '?')}"
        bar = Bar(
            id=str(_req(b_raw, "id", bwhere)),
            one_line=str(_req(b_raw, "one_line", bwhere)),
            derives_from=str(_req(b_raw, "derives_from", bwhere)),
            status=str(_req(b_raw, "status", bwhere)),
        )
        if bar.id in seen:
            raise DataError(f"{where}: bar id {bar.id!r} declared twice — one key, "
                            "one bar (§7.5)")
        seen.add(bar.id)
        if bar.status not in BAR_STATUS:
            raise DataError(f"{bwhere}: status {bar.status!r} not in {BAR_STATUS}")
        if "floor" in b_raw:
            bar.floor = _num(b_raw["floor"], "floor", bwhere)
            bar.floor_basis = str(b_raw.get("floor_basis", ""))
            if not bar.floor_basis:
                raise DataError(
                    f"{bwhere}: `floor` declared with no `floor_basis`. Name where "
                    "the number came from — a measurement with its date, or "
                    "\"structural\" — or drop the floor and be target-only.")
        if "target" in b_raw:
            bar.target = _num(b_raw["target"], "target", bwhere)
        bar.direction = str(b_raw.get("direction", "higher_is_better"))
        if bar.direction not in DIRECTIONS:
            raise DataError(f"{bwhere}: direction {bar.direction!r} not in {DIRECTIONS}")
        if "noise_band" in b_raw:
            bar.noise_band = _num(b_raw["noise_band"], "noise_band", bwhere)
        bar.instrument = str(b_raw.get("instrument", ""))
        bar.kill = str(b_raw.get("kill", ""))
        if "timeout_s" in b_raw:
            t = b_raw["timeout_s"]
            if isinstance(t, bool) or not isinstance(t, int) or t <= 0:
                raise DataError(f"{bwhere}: timeout_s = {t!r} must be a positive integer")
            if not bar.instrument:
                raise DataError(f"{bwhere}: timeout_s without an instrument — the "
                                "tier is inferred from timeout_s, and there is "
                                "nothing here to time out")
            bar.timeout_s = t
        if bar.instrument and bar.floor is None and bar.target is None:
            raise DataError(
                f"{bwhere}: instrument declared with neither floor nor target — a "
                "value nothing judges is telemetry theater. Declare a threshold "
                "(operator-ratified, §18.6) or drop the instrument.")
        camp.bars.append(bar)
    return camp


def load_campaigns(base: Path = CAMPAIGNS_DIR) -> list[Campaign]:
    if not base.is_dir():
        raise DataError(f"{base} does not exist — no campaign declares any bars")
    out: list[Campaign] = []
    seen: set[str] = set()
    for p in sorted(base.glob("*.toml")):      # closed/ is skipped by construction
        c = load_campaign_file(p)
        if c.id in seen:
            raise DataError(f"campaign id {c.id!r} declared by two files under {base}")
        seen.add(c.id)
        out.append(c)
    return out


def load_declaration(base: Path = CAMPAIGNS_DIR):
    """Compat surface for importers (co-order.sh's serves-check): returns
    (vocabulary-placeholder, campaigns, raw-placeholder). Campaigns carry .id
    and .bars[].id, which is all the serves-join reads."""
    return None, load_campaigns(base), {}


FRONT_RE = re.compile(r"\A---\n(.*?)\n---", re.S)


def parse_order(path: Path) -> Order | None:
    text = path.read_text(encoding="utf-8", errors="replace")
    m = FRONT_RE.match(text)
    if not m:
        return None
    fields: dict[str, str] = {}
    for line in m.group(1).splitlines():
        if line.lstrip().startswith("#"):
            continue
        if ":" not in line:
            continue
        k, _, v = line.partition(":")
        fields[k.strip()] = v.strip()
    serves_raw = fields.get("serves")
    return Order(
        id=fields.get("id", path.parent.name),
        path=path,
        status=fields.get("status", "unknown"),
        drafted=fields.get("drafted", ""),
        approved=fields.get("approved", ""),
        serves_raw=serves_raw,
        serves_initiative=_serves_initiative(serves_raw),
        serves_bars=_serves_bars(serves_raw),
    )


def _serves_tokens(serves_raw: str | None) -> list[str]:
    if not serves_raw:
        return []
    cleaned = serves_raw.split("#", 1)[0].strip()
    if not cleaned or cleaned.startswith("("):
        return []  # (unattributed) and friends — a legal, visible state
    # Everything from the first "(" on is a note to the reader, never a bar id.
    # Stripping it only when it LED the value meant `noun-convergence (instrument;
    # mints the numbers §10 cannot stand behind)` rendered eight phantom bars the
    # campaign never declared — "mints", "the", "numbers", "§10" ... (§18.3:
    # absence is reported, not invented). Bar ids never contain "(".
    cleaned = cleaned.split("(", 1)[0].strip()
    if not cleaned:
        return []
    return cleaned.replace(",", " ").split()


def _serves_initiative(serves_raw: str | None) -> str | None:
    toks = _serves_tokens(serves_raw)
    return toks[0] if toks else None


def _serves_bars(serves_raw: str | None) -> list[str]:
    return _serves_tokens(serves_raw)[1:]


def load_orders(features: Path = FEATURES) -> list[Order]:
    out = []
    if not features.is_dir():
        return out
    for p in sorted(features.glob("*/order.md")):
        o = parse_order(p)
        if o is not None:
            out.append(o)
    return out


# --------------------------------------------------------------------------
# the decider — ONE implementation (§10.6). Instruments never self-judge.
# --------------------------------------------------------------------------


def meets(value: float, threshold: float, direction: str) -> bool:
    if direction == "higher_is_better":
        return value >= threshold
    if direction == "lower_is_better":
        return value <= threshold
    return abs(value) <= threshold          # near_zero


def measured_verdict(value: float, bar: Bar) -> str:
    """target met -> met; else floor met -> met-floor (met when floor-only);
    else failed. noise_band grants NO verdict credit (§18.6) — it exists only
    to flatten trend."""
    if bar.target is not None and meets(value, bar.target, bar.direction):
        return "met"
    if bar.floor is not None and meets(value, bar.floor, bar.direction):
        return "met" if bar.target is None else "met-floor"
    return "failed"


# --------------------------------------------------------------------------
# the measurement store — append-only jsonl; verdict = newest row
# --------------------------------------------------------------------------


def read_measurements(store: Path = STORE) -> tuple[list[dict], int]:
    """-> (rows, malformed_count). A malformed row is COUNTED and named in the
    render's honesty pass, never silently dropped (§18.3)."""
    rows: list[dict] = []
    malformed = 0
    if not store.exists():
        return rows, 0
    for line in store.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            d = json.loads(line)
        except json.JSONDecodeError:
            malformed += 1
            continue
        if not isinstance(d, dict) or d.get("kind") != "bar-measurement":
            continue                        # other kinds share nothing here
        if (d.get("verdict") not in VERDICTS or not d.get("ts")
                or not d.get("bar") or not d.get("campaign")
                or d.get("ref_source") not in REF_SOURCES
                or not REASON_RE.match(d.get("reason", "x"))):
            malformed += 1
            continue
        rows.append(d)
    return rows, malformed


def rows_for_bar(rows: list[dict], campaign_id: str, bar_id: str) -> list[dict]:
    mine = [r for r in rows if r["campaign"] == campaign_id and r["bar"] == bar_id]
    mine.sort(key=lambda r: r["ts"])
    return mine


def verdict_of_bar(rows: list[dict], campaign_id: str, bar: Bar) -> str:
    """Newest measurement row wins; no rows = never-attempted. The ONLY
    implementation of this rule."""
    mine = rows_for_bar(rows, campaign_id, bar.id)
    return mine[-1]["verdict"] if mine else NEVER


def is_open(rows: list[dict], campaign_id: str, bar: Bar) -> bool:
    """OPEN unless measured `met` or status descoped. never-attempted counts
    as OPEN — a bar with no rows has not passed quietly."""
    if bar.status == "descoped":
        return False
    return verdict_of_bar(rows, campaign_id, bar) != "met"


def covering_orders(bar: Bar, camp: Campaign, orders: list[Order]) -> list[Order]:
    return [o for o in orders
            if o.serves_initiative == camp.id and bar.id in o.serves_bars]


def campaign_orders(camp: Campaign, orders: list[Order]) -> list[Order]:
    return [o for o in orders if o.serves_initiative == camp.id]


def substrate_epoch(rows: list[dict]) -> str:
    """Date of the FIRST measurement row ever written, or '' if there are none.

    Before it, no order COULD have been measured — the substrate did not exist.
    One decider for "was this order judgeable at all" (§10.6); both the per-bar
    flag and the campaign-level line read it.
    """
    return min((r.get("ts", "")[:10] for r in rows if r.get("ts")), default="")


def landed_but_unmoved(bar: Bar, camp: Campaign, orders: list[Order],
                       rows: list[dict]) -> list[Order]:
    """Covering orders that LANDED while the bar has no measurement rows at
    all, or none since the order's drafted date.

    Two exclusions, because in both an order is being blamed for a gap that is
    not its doing, and in both the fact is already reported elsewhere:

    - A bar with NO INSTRUMENT cannot be moved by any order. Flagging its
      covering orders restates the UNMEASURED line once per order — and a flag
      no action clears becomes wallpaper, which is how the next real one gets
      missed. UNMEASURED is the report.
    - An order drafted before `substrate_epoch` closed before measurement rows
      existed at all. Reported once at campaign level (`pre_substrate_orders`).
    """
    if not bar.instrument:
        return []
    epoch = substrate_epoch(rows)
    mine = rows_for_bar(rows, camp.id, bar.id)
    out = []
    for o in covering_orders(bar, camp, orders):
        if o.status != "landed":
            continue
        if epoch and o.drafted and o.drafted < epoch:
            continue
        since = [r for r in mine if not o.drafted or r["ts"][:10] >= o.drafted]
        if not since:
            out.append(o)
    return out


def pre_substrate_orders(camp: Campaign, orders: list[Order],
                         rows: list[dict]) -> list[Order]:
    """Landed orders that closed before the first measurement row existed.

    Not a finding about the order — a finding about WHEN the substrate arrived.
    Reported once, with the epoch, so the count is visible without five
    unactionable per-bar flags standing in for it.
    """
    epoch = substrate_epoch(rows)
    if not epoch:
        return []
    seen, out = set(), []
    for b in camp.bars:
        for o in covering_orders(b, camp, orders):
            if (o.status == "landed" and o.drafted and o.drafted < epoch
                    and o.id not in seen):
                seen.add(o.id)
                out.append(o)
    return out


# --------------------------------------------------------------------------
# measure — dumb, stdlib, daemon-free. Write-time git only.
# --------------------------------------------------------------------------


def _git_head_dirty() -> tuple[str | None, bool]:
    try:
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                              capture_output=True, text=True, timeout=10)
        status = subprocess.run(["git", "status", "--porcelain"], cwd=REPO,
                                capture_output=True, text=True, timeout=10)
    except Exception:
        return None, False
    sha = head.stdout.strip() if head.returncode == 0 else None
    return sha, bool(status.stdout.strip()) if status.returncode == 0 else False


def _run_instrument(instrument: str, timeout_s: int) -> tuple[int | None, str, str, bool, bool]:
    """-> (rc, stdout, stderr, timed_out, child_group_dead).

    start_new_session + killpg: a hanging instrument is killed as a GROUP —
    the runner cannot hang, and the kill is verified, not assumed."""
    proc = subprocess.Popen(["/bin/bash", "-c", instrument], cwd=REPO,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, start_new_session=True)
    timed_out = False
    try:
        out, err = proc.communicate(timeout=timeout_s)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except PermissionError:
            # EPERM: the group exists but is not ours to signal. Fall back to the
            # leader; the group check below will report honestly if it survives.
            try:
                proc.kill()
            except ProcessLookupError:
                pass
        out, err = proc.communicate()
    dead = True
    if timed_out:
        # Verify the process GROUP is gone, not merely that the leader was
        # reaped (a grandchild `sleep` survives a naive kill).
        import time as _time
        dead = False
        for _ in range(20):
            try:
                os.killpg(proc.pid, 0)
            except ProcessLookupError:
                dead = True
                break
            except PermissionError:
                # EPERM means the process group EXISTS and we may not signal it.
                # That is the opposite of dead, and it must not crash the sweep:
                # before this arm, one slow instrument aborted the whole
                # `measure` run with a traceback instead of recording a
                # could-not-judge row for the bar that was actually at fault.
                dead = False
                break
            _time.sleep(0.01)
    return proc.returncode, out or "", err or "", timed_out, dead


def _parse_value(stdout_text: str) -> dict | None:
    """Last non-empty stdout line: bare number, or JSON {"value": N, ...}."""
    lines = [ln.strip() for ln in stdout_text.splitlines() if ln.strip()]
    if not lines:
        return None
    last = lines[-1]
    if last.startswith("{"):
        try:
            d = json.loads(last)
        except json.JSONDecodeError:
            return None
        v = d.get("value")
        if isinstance(v, bool) or not isinstance(v, (int, float)):
            return None
        return {"value": float(v), "commit": d.get("commit"),
                "artifact": d.get("artifact")}
    try:
        return {"value": float(last), "commit": None, "artifact": None}
    except ValueError:
        return None


def _now_iso() -> str:
    return _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds")


def measure_bar(camp: Campaign, bar: Bar, head: str | None, dirty: bool,
                log_dir: Path = MEASURE_LOG_DIR) -> dict:
    """Run one bar's instrument and build its measurement row. Pure record:
    a failed verdict is a row, never an exit code."""
    ts = _now_iso()
    stamp = ts.replace(":", "").replace("+00:00", "Z")
    log_dir.mkdir(parents=True, exist_ok=True)
    log_path = log_dir / f"{stamp}.{bar.id}.log"
    cap = bar.timeout_s if bar.run_tier else READ_TIER_TIMEOUT_S
    rc, out, err, timed_out, dead = _run_instrument(bar.instrument, cap)

    value = None
    reason = ""
    artifact = None
    commit = None
    if timed_out:
        verdict, reason = "could-not-judge", f"timeout {cap}s"
    elif rc == 127:
        verdict, reason = "could-not-judge", "instrument-missing"
    elif rc == 3:
        verdict, reason = "could-not-judge", "artifact-absent"
    elif rc != 0:
        verdict, reason = "could-not-judge", f"exit {rc}"
    else:
        parsed = _parse_value(out)
        if parsed is None:
            verdict, reason = "could-not-judge", "unparseable-output"
        else:
            value = parsed["value"]
            commit = parsed["commit"]
            artifact = parsed["artifact"]
            verdict = measured_verdict(value, bar)

    if bar.run_tier:
        ref, ref_source = head, ("head" if head else "unattributed")
    elif commit:
        ref, ref_source = commit, "artifact"
    else:
        ref, ref_source = None, "unattributed"

    artifact_mtime = None
    if artifact:
        ap = Path(artifact)
        if not ap.is_absolute():
            ap = REPO / ap
        if ap.exists():
            artifact_mtime = int(ap.stat().st_mtime)

    log_path.write_text(
        f"# co-lineage measure  ts={ts}  campaign={camp.id}  bar={bar.id}\n"
        f"# instrument: {bar.instrument}\n"
        f"# rc={rc} timed_out={timed_out} child_group_dead={dead} cap={cap}s\n"
        f"# verdict={verdict} value={value} reason={reason!r}\n"
        f"--- stdout ---\n{out}\n--- stderr ---\n{err}\n", encoding="utf-8")

    return {
        "kind": "bar-measurement", "ts": ts, "campaign": camp.id, "bar": bar.id,
        "value": value, "verdict": verdict, "reason": reason,
        "ref": ref, "ref_source": ref_source,
        "artifact": artifact, "artifact_mtime": artifact_mtime,
        "dirty": dirty, "exit": rc,
        "floor": bar.floor, "target": bar.target, "direction": bar.direction,
        "noise_band": bar.noise_band,
        "log": str(log_path), "engine": "co-lineage measure",
    }


def measure_campaign(camp: Campaign, store: Path = STORE,
                     log_dir: Path = MEASURE_LOG_DIR, out=sys.stdout) -> int:
    p = lambda s="": print(s, file=out)  # noqa: E731
    head, dirty = _git_head_dirty()
    if (REPO / ".git").is_file():
        p(f"measure: WARNING — {REPO} is a worktree; measure from the main "
          "checkout so refs attribute to the history peers can see")
    instrumented = [b for b in camp.bars if b.instrument]
    skipped = [b for b in camp.bars if not b.instrument]
    wrote = 0
    for bar in instrumented:
        row = measure_bar(camp, bar, head, dirty, log_dir=log_dir)
        try:
            store.parent.mkdir(parents=True, exist_ok=True)
            with open(store, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(row) + "\n")
        except OSError as exc:
            p(f"measure: store unwritable ({exc}) — refusing to pretend a row landed")
            return 5
        wrote += 1
        v = row["verdict"]
        detail = f"value={row['value']}" if row["value"] is not None else f"reason={row['reason']}"
        p(f"measure: {camp.id}/{bar.id}  {v}  {detail}  ref={row['ref_source']}"
          + ("  DIRTY" if dirty else "") + f"  log={row['log']}")
        if v == "met-floor":
            p(f"  met-floor -> file the tuning debt (a human/seat act in v1):\n"
              f"  scripts/co-backlog-producer.sh --key {bar.id} "
              f"--title 'above floor, below target: {camp.id}/{bar.id}' "
              f"--objective {camp.id} --evidence-file {row['log']}")
    if skipped:
        p(f"measure: {len(skipped)} bar(s) uninstrumented, no row written "
          f"({', '.join(b.id for b in skipped)}) — they render UNMEASURED")
    if wrote != len(instrumented):
        return 5
    return 0                                # telemetry records, never gates


# --------------------------------------------------------------------------
# render helpers — trend, age, annotations
# --------------------------------------------------------------------------


def _parse_ts(ts: str) -> _dt.datetime | None:
    try:
        d = _dt.datetime.fromisoformat(ts.replace("Z", "+00:00"))
        return d if d.tzinfo else d.replace(tzinfo=_dt.timezone.utc)
    except ValueError:
        return None


def _age_label(ts: str, now: _dt.datetime) -> tuple[str, bool]:
    """-> (label, stale). Age from row ts — no stat, no git at read time."""
    d = _parse_ts(ts)
    if d is None:
        return "age unknown", True
    hours = (now - d).total_seconds() / 3600
    stale = hours > STALE_HOURS
    if hours < 1:
        return f"age {int(hours * 60)}m", stale
    if hours < STALE_HOURS:
        return f"age {int(hours)}h", stale
    return f"age {hours / 24:.1f}d", stale


def trend_of(mine: list[dict]) -> str:
    """Judged over the last <=TREND_N valued rows against the NEWEST row's
    snapshot noise_band (§18.5). No band -> raw delta, named unjudged."""
    valued = [r for r in mine if isinstance(r.get("value"), (int, float))][-TREND_N:]
    n = len(valued)
    if n < 2:
        return f"trend n={n} (insufficient)"
    newest, oldest = valued[-1], valued[0]
    delta = newest["value"] - oldest["value"]
    band = newest.get("noise_band")
    direction = newest.get("direction", "higher_is_better")
    if band is None:
        return f"trend delta {delta:+.4g} over n={n} (delta unjudged — no noise_band)"
    if direction == "near_zero":
        moved = abs(newest["value"]) - abs(oldest["value"])
        if abs(moved) <= band:
            return f"trend ~flat(n={n})"
        return f"trend {'WORSENING' if moved > 0 else 'improving'}(n={n})"
    if abs(delta) <= band:
        return f"trend ~flat(n={n})"
    good = delta > 0 if direction == "higher_is_better" else delta < 0
    return f"trend {'improving' if good else 'WORSENING'}(n={n})"


def _fmt_thr(bar: Bar) -> str:
    parts = []
    if bar.floor is not None:
        parts.append(f"floor {bar.floor:g}")
    if bar.target is not None:
        parts.append(f"target {bar.target:g}")
    return " ".join(parts) or "no thresholds"


def _snapshot_moved(row: dict, bar: Bar) -> list[str]:
    """§18.6 audit trail: the row snapshots what it was judged against; a
    divergence from the file NOW is shouted, never smoothed."""
    out = []
    for key, current in (("floor", bar.floor), ("target", bar.target),
                         ("direction", bar.direction), ("noise_band", bar.noise_band)):
        was = row.get(key)
        if was != current:
            out.append(f"TARGET MOVED MID-CAMPAIGN: row judged against {key}={was!r}, "
                       f"file now says {current!r} (§18.6 — only the operator moves one)")
    return out


def _measured_lines(bar: Bar, camp: Campaign, rows: list[dict],
                    now: _dt.datetime) -> list[str]:
    mine = rows_for_bar(rows, camp.id, bar.id)
    if not mine:
        if not bar.instrument:
            return ["measured: UNMEASURED — no instrument declared"]
        return ["measured: NEVER-MEASURED (instrument declared, no rows)"]
    newest = mine[-1]
    age, stale = _age_label(newest["ts"], now)
    head = f"measured: {newest['verdict']}"
    if newest.get("value") is not None:
        head += f" {newest['value']:g}"
    if newest.get("reason"):
        head += f" [{newest['reason']}]"
    head += f" ({_fmt_thr(bar)}) {trend_of(mine)} {age}"
    lines = [head]
    if stale:
        lines.append(f"!! STALE — newest row is past the {STALE_HOURS}h line")
    if newest.get("dirty"):
        lines.append("!! DIRTY tree at measure time — the ref does not fully "
                     "describe what ran")
    if newest.get("ref_source") == "unattributed":
        lines.append("!! ref UNATTRIBUTED — no commit recoverable for this value; "
                     "it claims no HEAD (§18.3)")
    if (len(mine) >= 2 and newest.get("artifact")
            and newest.get("artifact") == mine[-2].get("artifact")
            and newest.get("artifact_mtime") is not None
            and newest.get("artifact_mtime") == mine[-2].get("artifact_mtime")):
        lines.append("!! STATIC ARTIFACT — same artifact, same mtime as the "
                     "previous row: the instrument re-read a dead artifact (§18.4)")
    lines.extend("!! " + s for s in _snapshot_moved(newest, bar))
    return lines


# --------------------------------------------------------------------------
# drift block — counts + sha pointers only; a model never authors a line
# --------------------------------------------------------------------------


def read_drift_rows(store: Path = DRIFT_STORE) -> list[dict]:
    rows: list[dict] = []
    if not store.exists():
        return rows
    for line in store.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            d = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(d, dict) and d.get("kind") == "drift":
            rows.append(d)
    return rows


def _drift_block(camp: Campaign, orders: list[Order], drift: list[dict],
                 now: _dt.datetime, p) -> None:
    p()
    if not drift:
        p("drift (shadow): no rows — monitor last ran never")
        return
    newest = max((r.get("ts", "") for r in drift), default="")
    age, _ = _age_label(newest, now) if newest else ("age unknown", True)
    order_ids = {o.id for o in campaign_orders(camp, orders)}
    mine = [r for r in drift if r.get("order") in order_ids
            or r.get("serves", "").split()[:1] == [camp.id]]
    if not mine:
        p(f"drift (shadow): no rows for this campaign — monitor last ran {age}")
        return
    by = {}
    for r in mine:
        by[r.get("verdict", "?")] = by.get(r.get("verdict", "?"), 0) + 1
    p(f"drift (shadow, {len(mine)} row(s), monitor last ran {age}): "
      + "  ".join(f"{k} {v}" for k, v in sorted(by.items())))
    flagged = [r for r in mine if r.get("verdict") in ("unsupported", "out-of-scope")]
    for r in flagged[-8:]:
        p(f"  {r.get('verdict'):<13} {str(r.get('ref', ''))[:9]}  "
          f"{str(r.get('claim', ''))[:70]}")


# --------------------------------------------------------------------------
# render — deterministic coverage view; no model authors a rendered line
# --------------------------------------------------------------------------


def _wrap(text: str, width: int, indent: str) -> list[str]:
    import textwrap
    flat = " ".join((text or "").split())
    if not flat:
        return []
    return textwrap.wrap(flat, width=width, initial_indent=indent,
                         subsequent_indent=indent)


def render_coverage(camp: Campaign, orders: list[Order], rows: list[dict],
                    malformed: int, drift: list[dict],
                    now: _dt.datetime | None = None, out=sys.stdout) -> None:
    p = lambda s="": print(s, file=out)  # noqa: E731
    now = now or _dt.datetime.now(_dt.timezone.utc)
    mine = campaign_orders(camp, orders)

    p(f"campaign: {camp.id} — {camp.objective}")
    p(f"spec:     {camp.spec or '(none declared)'}")
    p(f"declared: {camp.declared}   status: {camp.status}   bars: {len(camp.bars)}"
      f"/{MAX_BARS}   orders serving it: {len(mine)}")
    p()

    uncovered = [b for b in camp.bars if not covering_orders(b, camp, orders)]
    open_bars = [b for b in camp.bars if is_open(rows, camp.id, b)]
    unmeasured = [b for b in camp.bars
                  if not b.instrument and not rows_for_bar(rows, camp.id, b.id)]
    never_measured = [b for b in camp.bars
                      if b.instrument and not rows_for_bar(rows, camp.id, b.id)]
    p(f"UNCOVERED BARS = {len(uncovered)} of {len(camp.bars)}"
      + (f"   ({', '.join(b.id for b in uncovered)})" if uncovered else ""))
    p(f"OPEN BARS = {len(open_bars)} of {len(camp.bars)}   "
      "(never-attempted counts as open)")
    if unmeasured:
        p(f"UNMEASURED = {len(unmeasured)}   ({', '.join(b.id for b in unmeasured)})"
          "   — no instrument declared; nothing can move these but a data edit")
    if never_measured:
        p(f"NEVER-MEASURED = {len(never_measured)}   "
          f"({', '.join(b.id for b in never_measured)})   — instrument declared, no rows")
    pre = pre_substrate_orders(camp, orders, rows)
    if pre:
        p(f"PRE-SUBSTRATE = {len(pre)} landed order(s) closed before the first "
          f"measurement row ({substrate_epoch(rows)})   "
          f"({', '.join(o.id for o in pre)})")
        p("   — not judgeable, and no action clears them; excluded from the "
          "per-bar LANDED-BUT-UNMOVED flag")
    p()

    for b in camp.bars:
        cov = covering_orders(b, camp, orders)
        v = verdict_of_bar(rows, camp.id, b)
        cov_label = f"covered({len(cov)})" if cov else "UNCOVERED"
        p(f"{b.id:<30} [{b.status}]  {cov_label:<12} {v}")
        for line in _wrap(b.one_line, 76, " " * 4):
            p(line)
        for line in _measured_lines(b, camp, rows, now):
            p(" " * 4 + line)
        if cov:
            p(" " * 4 + "orders: " + ", ".join(f"{o.id}({o.status})" for o in cov))
        stuck = landed_but_unmoved(b, camp, orders, rows)
        if stuck:
            p(" " * 4 + ">> LANDED-BUT-UNMOVED: " + ", ".join(o.id for o in stuck)
              + " closed `landed` with no measurement row since drafting")
    p()
    counts = {k: 0 for k in VERDICTS + (NEVER,)}
    for b in camp.bars:
        counts[verdict_of_bar(rows, camp.id, b)] += 1
    p("verdict summary: " + "  ".join(f"{k} {v}" for k, v in counts.items()))

    _drift_block(camp, orders, drift, now, p)

    # honesty pass — what this view could NOT map, stated, never dropped (§18.3)
    p()
    p("what this view could not map:")
    problems: list[str] = []
    if malformed:
        problems.append(f"{malformed} malformed row(s) in the measurement store — "
                        "counted, not silently skipped")
    known = {b.id for b in camp.bars}
    for o in mine:
        for bid in o.serves_bars:
            if bid not in known:
                problems.append(f"order {o.id} serves bar {bid!r}, which "
                                f"{camp.path.name} does not declare")
    if camp.spec and not (REPO / camp.spec).exists():
        problems.append(f"campaign spec {camp.spec} does not resolve in this tree")
    for b in never_measured:
        problems.append(f"bar {b.id} declares an instrument but has never been "
                        "measured — declared telemetry that never ran")
    if not problems:
        p("  - every declared bar maps; every serves: id is declared; store clean")
    for x in problems:
        p(f"  - {x}")


def render_postmortem(camp: Campaign, orders: list[Order], rows: list[dict],
                      out=sys.stdout) -> None:
    p = lambda s="": print(s, file=out)  # noqa: E731
    p("=" * 90)
    p(f"POST-MORTEM — {camp.id} — {camp.objective}")
    p(f"declared {camp.declared}   status {camp.status}")
    p("=" * 90)
    for b in camp.bars:
        mine = rows_for_bar(rows, camp.id, b.id)
        p()
        p(f"  {b.id}  [{b.status}]  {verdict_of_bar(rows, camp.id, b)}")
        for line in _wrap(b.one_line, 84, "      "):
            p(line)
        if not mine:
            p("      no measurement rows — never attempted")
        for r in mine:
            val = f"{r['value']:g}" if r.get("value") is not None else f"[{r['reason']}]"
            p(f"        {r['ts']}  {r['verdict']:<16} {val:<12} "
              f"ref={str(r.get('ref') or '-')[:9]}({r['ref_source']})"
              + ("  DIRTY" if r.get("dirty") else ""))
    p()
    p("  status-edit ledger (git log --follow of the campaign file):")
    try:
        log = subprocess.run(
            ["git", "log", "--follow", "--oneline", "--", str(camp.path)],
            cwd=REPO, capture_output=True, text=True, timeout=15)
        for line in (log.stdout or "").splitlines()[:20]:
            p(f"    {line}")
    except Exception as exc:  # noqa: BLE001 — postmortem is best-effort on git
        p(f"    (git unavailable: {exc})")


# --------------------------------------------------------------------------
# self-test — a gate you have not watched fail is not a gate (§18.1)
# --------------------------------------------------------------------------

FIXTURE_CAMPAIGN = """\
id        = "t"
objective = "fixture"
spec      = "quality/campaigns"
declared  = "2026-01-01"
status    = "active"
"""


def _bar_toml(bar_id="B", floor=4.0, target=10.0, direction="higher_is_better",
              instrument="", timeout_s=None, floor_basis="measured 4 on fixture",
              noise_band=None, status="open", drop=()):
    lines = ["[[bar]]", f'id = "{bar_id}"', 'one_line = "x"',
             'derives_from = "spec §1"', f'status = "{status}"']
    if floor is not None:
        lines.append(f"floor = {floor}")
        if floor_basis:
            lines.append(f'floor_basis = "{floor_basis}"')
    if target is not None:
        lines.append(f"target = {target}")
    lines.append(f'direction = "{direction}"')
    if noise_band is not None:
        lines.append(f"noise_band = {noise_band}")
    if instrument:
        lines.append(f"instrument = '''{instrument}'''")
    if timeout_s is not None:
        lines.append(f"timeout_s = {timeout_s}")
    return "\n".join(ln for ln in lines if not any(ln.startswith(d) for d in drop)) + "\n"


def self_test() -> int:  # noqa: C901 — a flat checklist reads better than a framework
    import io
    import tempfile

    failures: list[str] = []

    def check(name: str, cond: bool, detail: str = "") -> None:
        if cond:
            print(f"  pass  {name}")
        else:
            print(f"  FAIL  {name}  {detail}")
            failures.append(name)

    def load_text(text: str) -> Campaign:
        with tempfile.TemporaryDirectory() as td:
            f = Path(td) / "t.toml"
            f.write_text(text, encoding="utf-8")
            return load_campaign_file(f)

    # serves: a parenthetical is a note to the reader, never a bar id (§18.3)
    check("serves: bar ids parse", _serves_initiative("nc x") == "nc" and _serves_bars("nc x") == ["x"])
    check("serves: trailing (unattributed) is not a bar",
          _serves_bars("nc (unattributed)") == [], _serves_bars("nc (unattributed)"))
    check("serves: multi-word parenthetical yields no phantom bars",
          _serves_bars("nc (instrument; mints the numbers)") == [],
          _serves_bars("nc (instrument; mints the numbers)"))
    check("serves: leading parenthetical yields no initiative",
          _serves_initiative("(unattributed)") is None)

    def expect_error(name: str, text: str, needle: str) -> None:
        try:
            load_text(text)
            check(name, False, "loader accepted it")
        except DataError as exc:
            check(name, needle in str(exc), str(exc))

    print("co-lineage --self-test (campaign substrate)")

    # ---- loader: every new rule proven to raise DataError -----------------
    ten_bars = FIXTURE_CAMPAIGN + "".join(_bar_toml(f"B{i}") for i in range(10))
    expect_error("the 9-bar cap is a LOAD ERROR at 10", ten_bars, "cap")
    expect_error("unknown bar status is rejected",
                 FIXTURE_CAMPAIGN + _bar_toml(status="paused"), "status")
    expect_error("unknown campaign status is rejected",
                 FIXTURE_CAMPAIGN.replace('"active"', '"zombie"') + _bar_toml(), "status")
    expect_error("a floor with no floor_basis is rejected",
                 FIXTURE_CAMPAIGN + _bar_toml(floor_basis=""), "floor_basis")
    expect_error("an instrument with neither floor nor target is rejected",
                 FIXTURE_CAMPAIGN + _bar_toml(floor=None, target=None,
                                              instrument="echo 1"), "neither floor nor target")
    expect_error("a non-numeric target is rejected",
                 FIXTURE_CAMPAIGN + _bar_toml().replace("target = 10.0",
                                                        'target = "high"'), "not numeric")
    expect_error("timeout_s without an instrument is rejected",
                 FIXTURE_CAMPAIGN + _bar_toml() + "timeout_s = 5\n", "without an instrument")
    expect_error("a non-positive timeout_s is rejected",
                 FIXTURE_CAMPAIGN + _bar_toml(instrument="echo 1") + "timeout_s = 0\n",
                 "positive")
    expect_error("a missing required key is DataError, not KeyError",
                 FIXTURE_CAMPAIGN + "[[bar]]\nid = \"B\"\nstatus = \"open\"\n",
                 "one_line")
    expect_error("an unknown direction is rejected",
                 FIXTURE_CAMPAIGN + _bar_toml(direction="sideways"), "direction")
    expect_error("a duplicate bar id is rejected",
                 FIXTURE_CAMPAIGN + _bar_toml("B1") + _bar_toml("B1"), "twice")
    nine = FIXTURE_CAMPAIGN + "".join(_bar_toml(f"B{i}") for i in range(9))
    check("9 bars load clean (the cap binds at 10, not 9)",
          len(load_text(nine).bars) == 9)

    # ---- the decider: truth table, one implementation ---------------------
    hi = load_text(FIXTURE_CAMPAIGN + _bar_toml()).bars[0]          # floor 4 target 10
    check("higher: 12 -> met", measured_verdict(12, hi) == "met")
    check("higher: 10 (boundary) -> met", measured_verdict(10, hi) == "met")
    check("higher: 5 -> met-floor", measured_verdict(5, hi) == "met-floor")
    check("higher: 4 (boundary) -> met-floor", measured_verdict(4, hi) == "met-floor")
    check("higher: 3.9 -> failed", measured_verdict(3.9, hi) == "failed")
    check("A BELOW-FLOOR VALUE IS NEVER met", measured_verdict(2, hi) != "met",
          "the one lie this decider must be unable to tell")
    lo = load_text(FIXTURE_CAMPAIGN + _bar_toml(
        floor=100, target=50, direction="lower_is_better")).bars[0]
    check("lower: 40 -> met", measured_verdict(40, lo) == "met")
    check("lower: 50 (boundary) -> met", measured_verdict(50, lo) == "met")
    check("lower: 80 -> met-floor", measured_verdict(80, lo) == "met-floor")
    check("lower: 101 -> failed", measured_verdict(101, lo) == "failed")
    nz = load_text(FIXTURE_CAMPAIGN + _bar_toml(
        floor=None, target=0.05, direction="near_zero")).bars[0]
    check("near_zero: 0.04 in -> met", measured_verdict(0.04, nz) == "met")
    check("near_zero: -0.04 in -> met", measured_verdict(-0.04, nz) == "met")
    check("near_zero: 0.06 out -> failed", measured_verdict(0.06, nz) == "failed")
    tonly = load_text(FIXTURE_CAMPAIGN + _bar_toml(floor=None)).bars[0]
    check("target-only: below target -> failed, no yellow to hide in",
          measured_verdict(9, tonly) == "failed")
    fonly = load_text(FIXTURE_CAMPAIGN + _bar_toml(target=None)).bars[0]
    check("floor-only: above floor -> met", measured_verdict(5, fonly) == "met")
    check("floor-only: below floor -> failed", measured_verdict(3, fonly) == "failed")

    # ---- the runner: watch EVERY planted-bad fixture fail correctly -------
    # (order campaign-telemetry: four false greens in one day, every one
    #  exit 0 — this table is the acceptance criterion for trusting any row)
    with tempfile.TemporaryDirectory() as td:
        tdp = Path(td)
        store = tdp / "store.jsonl"
        logs = tdp / "logs"

        def measure_one(instrument, timeout_s=None, floor=4.0, target=10.0,
                        direction="higher_is_better"):
            camp = load_text(FIXTURE_CAMPAIGN + _bar_toml(
                floor=floor, target=target, direction=direction,
                instrument=instrument, timeout_s=timeout_s))
            rc = measure_campaign(camp, store=store, log_dir=logs, out=io.StringIO())
            rows, _ = read_measurements(store)
            return rc, rows[-1]

        rc, row = measure_one("echo 12")
        check("echo 12 -> met, exit 0", rc == 0 and row["verdict"] == "met"
              and row["value"] == 12, str(row))
        _, row = measure_one("echo 5")
        check("echo 5 -> met-floor", row["verdict"] == "met-floor")
        check("met-floor snapshots its thresholds (§18.6 audit trail)",
              row["floor"] == 4.0 and row["target"] == 10.0)
        rc, row = measure_one("echo 2")
        check("echo 2 -> failed AND measure still exits 0 (records, never gates)",
              rc == 0 and row["verdict"] == "failed")
        check("PLANTED BELOW-FLOOR ROW IS NEVER met", row["verdict"] != "met")
        _, row = measure_one("exit 3")
        check("exit 3 -> could-not-judge(artifact-absent)",
              row["verdict"] == "could-not-judge" and row["reason"] == "artifact-absent")
        _, row = measure_one("echo garbage")
        check("echo garbage -> could-not-judge(unparseable-output)",
              row["reason"] == "unparseable-output" and row["value"] is None)
        _, row = measure_one("/no/such/bin")
        check("/no/such/bin -> could-not-judge(instrument-missing)",
              row["reason"] == "instrument-missing", str(row))
        _, row = measure_one("exit 7")
        check("exit 7 -> could-not-judge(exit 7) — named, not defaulted",
              row["reason"] == "exit 7")

        rc_, out_, _err, timed_out, dead = _run_instrument("sleep 5", 1)
        check("sleep 5 under a 1s cap -> timed out AND child group verified DEAD",
              timed_out and dead, f"timed_out={timed_out} dead={dead}")
        _, row = measure_one("sleep 5", timeout_s=1)
        check("timeout row -> could-not-judge(timeout 1s)",
              row["verdict"] == "could-not-judge" and row["reason"] == "timeout 1s")

        art = tdp / "artifact.json"
        art.write_text("{}")
        _, row = measure_one(
            f'echo \'{{"value": 7, "commit": "abc1234", "artifact": "{art}"}}\'')
        check("JSON output -> value, ref and artifact_mtime recorded",
              row["value"] == 7 and row["ref"] == "abc1234"
              and row["ref_source"] == "artifact"
              and row["artifact_mtime"] is not None, str(row))
        _, row = measure_one("echo 6")
        check("read-tier bare number -> ref UNATTRIBUTED, never a claimed HEAD",
              row["ref"] is None and row["ref_source"] == "unattributed")
        _, row = measure_one("echo 6", timeout_s=60)
        check("run-tier -> ref = HEAD at measure time, ref_source head",
              row["ref_source"] == "head" and row["ref"], str(row["ref"]))

        # ---- store reading, verdict-of-bar, malformed accounting ----------
        rows, malformed = read_measurements(store)
        check("every runner row above re-reads as well-formed", malformed == 0,
              f"malformed={malformed}")
        with open(store, "a") as fh:
            fh.write("this is not json\n")
            fh.write(json.dumps({"kind": "bar-measurement", "ts": "t",
                                 "campaign": "t", "bar": "B",
                                 "verdict": "sort-of-met",
                                 "ref_source": "head", "reason": ""}) + "\n")
        rows2, malformed2 = read_measurements(store)
        check("a malformed line and an out-of-set verdict are COUNTED, not read",
              malformed2 == 2 and len(rows2) == len(rows))

        camp = load_text(FIXTURE_CAMPAIGN + _bar_toml(instrument="echo 12"))
        bar = camp.bars[0]
        # build a two-row store and watch the newest row win
        s2 = tdp / "s2.jsonl"
        for ts, v in (("2026-01-01T00:00:00+00:00", "failed"),
                      ("2026-01-02T00:00:00+00:00", "met")):
            with open(s2, "a") as fh:
                fh.write(json.dumps({"kind": "bar-measurement", "ts": ts,
                                     "campaign": "t", "bar": "B", "value": 1,
                                     "verdict": v, "reason": "", "ref": None,
                                     "ref_source": "unattributed"}) + "\n")
        r2, _ = read_measurements(s2)
        check("newest row wins", verdict_of_bar(r2, "t", bar) == "met")
        check("no rows = never-attempted (the absence IS the verdict)",
              verdict_of_bar([], "t", bar) == NEVER)
        check("never-attempted counts as OPEN", is_open([], "t", bar))
        check("measured met closes the bar", not is_open(r2, "t", bar))
        desc = load_text(FIXTURE_CAMPAIGN + _bar_toml(status="descoped")).bars[0]
        check("descoped closes by decision", not is_open([], "t", desc))

        # ---- trend + render ----------------------------------------------
        def mk_row(ts, value, band=0.05, art=None, mtime=None):
            return {"kind": "bar-measurement", "ts": ts, "campaign": "t",
                    "bar": "B", "value": value, "verdict": "met-floor",
                    "reason": "", "ref": None, "ref_source": "unattributed",
                    "dirty": False, "floor": 4.0, "target": 10.0,
                    "direction": "higher_is_better", "noise_band": band,
                    "artifact": art, "artifact_mtime": mtime}

        flat = [mk_row(f"2026-01-0{i}T00:00:00+00:00", 5.0 + 0.005 * i)
                for i in range(1, 8)]
        check("trend within band -> ~flat(n=7)", "~flat(n=7)" in trend_of(flat))
        worse = [mk_row(f"2026-01-0{i}T00:00:00+00:00", 6.0 - 0.2 * i)
                 for i in range(1, 8)]
        check("trend against direction beyond band -> WORSENING",
              "WORSENING" in trend_of(worse))
        nb = [mk_row(f"2026-01-0{i}T00:00:00+00:00", 5.0 + i, band=None)
              for i in range(1, 4)]
        check("no noise_band -> raw delta named unjudged", "unjudged" in trend_of(nb))
        check("one row is not a trend (§18.5)", "insufficient" in trend_of(flat[:1]))

        camp_r = load_text(FIXTURE_CAMPAIGN
                           + _bar_toml("B", instrument="echo 5")
                           + _bar_toml("B-bare", floor=None, target=1.0))
        now = _dt.datetime(2026, 1, 10, tzinfo=_dt.timezone.utc)
        buf = io.StringIO()
        render_coverage(camp_r, [], flat, 0, [], now=now, out=buf)
        text = buf.getvalue()
        check("render: uninstrumented bar renders UNMEASURED loudly",
              "UNMEASURED" in text)
        check("render: rows older than 48h shout STALE", "STALE" in text)
        check("render: trend and age on the measured line",
              "~flat(n=7)" in text and "age" in text)
        buf2 = io.StringIO()
        render_coverage(camp_r, [], [], 0, [], now=now, out=buf2)
        check("render: instrument with no rows renders NEVER-MEASURED",
              "NEVER-MEASURED" in buf2.getvalue())
        moved = [dict(r, target=12.0) for r in flat]
        buf3 = io.StringIO()
        render_coverage(camp_r, [], moved, 0, [], now=now, out=buf3)
        check("render: snapshot != file thresholds shouts TARGET MOVED (§18.6)",
              "TARGET MOVED MID-CAMPAIGN" in buf3.getvalue())
        stat_rows = [mk_row("2026-01-09T00:00:00+00:00", 5.0, art="/tmp/a", mtime=111),
                     mk_row("2026-01-09T12:00:00+00:00", 5.0, art="/tmp/a", mtime=111)]
        buf4 = io.StringIO()
        render_coverage(camp_r, [], stat_rows, 0, [], now=now, out=buf4)
        check("render: unchanged artifact mtime across rows -> STATIC ARTIFACT (§18.4)",
              "STATIC ARTIFACT" in buf4.getvalue())
        check("render: unattributed ref is annotated by name (§18.3)",
              "UNATTRIBUTED" in buf4.getvalue())
        buf5 = io.StringIO()
        render_coverage(camp_r, [], flat, 3, [], now=now, out=buf5)
        check("render: malformed store rows are counted in the honesty pass",
              "3 malformed row(s)" in buf5.getvalue())
        buf6 = io.StringIO()
        render_coverage(camp_r, [], flat, 0, [], now=now, out=buf6)
        check("render: empty drift store says so, with monitor age",
              "drift (shadow): no rows" in buf6.getvalue())

        # ---- order join: coverage axis + landed-but-unmoved ---------------
        def fake_order(oid, status, serves, drafted="2026-01-02"):
            return Order(id=oid, path=Path(oid), status=status, drafted=drafted,
                         approved="x", serves_raw=serves,
                         serves_initiative=_serves_initiative(serves),
                         serves_bars=_serves_bars(serves))

        orders = [fake_order("o-landed", "landed", "t B"),
                  fake_order("o-ghost", "landed", "t B-nope")]
        check("covering_orders joins on campaign id + bar id",
              [o.id for o in covering_orders(bar, camp_r, orders)] == ["o-landed"])
        check("LANDED-BUT-UNMOVED fires when a landed order has no rows",
              [o.id for o in landed_but_unmoved(bar, camp_r, orders, [])]
              == ["o-landed"])
        check("a measurement row after drafting clears LANDED-BUT-UNMOVED",
              landed_but_unmoved(bar, camp_r, orders, flat) == [])

        # ---- an uninstrumented bar blames no order (UNMEASURED reports it) -
        import dataclasses as _dc
        bar_noinst = _dc.replace(bar, instrument="")
        check("a bar with NO instrument flags nobody — UNMEASURED is the report",
              landed_but_unmoved(bar_noinst, camp_r, orders, []) == []
              and [o.id for o in landed_but_unmoved(bar, camp_r, orders, [])]
              == ["o-landed"])

        # ---- the pre-substrate floor: reported once, never per bar --------
        check("substrate_epoch is the first row's date",
              substrate_epoch(flat) == "2026-01-01")
        check("no rows at all -> no epoch, so nothing is excluded",
              substrate_epoch([]) == ""
              and [o.id for o in landed_but_unmoved(bar, camp_r, orders, [])]
              == ["o-landed"])
        old = [fake_order("o-ancient", "landed", "t B", drafted="2025-12-31")]
        check("an order drafted BEFORE the first row is not flagged per bar",
              landed_but_unmoved(bar, camp_r, old, flat) == [])
        check("...and is reported once at campaign level instead",
              [o.id for o in pre_substrate_orders(camp_r, old, flat)]
              == ["o-ancient"])
        check("an order drafted AFTER the epoch is still flagged",
              [o.id for o in landed_but_unmoved(
                  bar, camp_r, [fake_order("o-recent", "landed", "t B",
                                           drafted="2026-06-01")], flat)]
              == ["o-recent"])
        buf8 = io.StringIO()
        render_coverage(camp_r, old, flat, 0, [], now=now, out=buf8)
        check("the render names the epoch and the excluded order",
              "PRE-SUBSTRATE" in buf8.getvalue()
              and "2026-01-01" in buf8.getvalue()
              and "o-ancient" in buf8.getvalue())
        buf7 = io.StringIO()
        render_coverage(camp_r, orders, [], 0, [], now=now, out=buf7)
        check("an undeclared bar id in serves: is reported, not dropped",
              "B-nope" in buf7.getvalue())

    # ---- the real campaign dir parses ------------------------------------
    try:
        camps = load_campaigns()
        check(f"quality/campaigns parses ({len(camps)} campaign(s), "
              f"{sum(len(c.bars) for c in camps)} bars)", True)
    except DataError as exc:
        check("quality/campaigns parses", False, str(exc))

    print()
    if failures:
        print(f"self-test: FAIL — {len(failures)} of the checks above did not hold")
        return 1
    print("self-test: pass — cap/closed-sets/thresholds error at load, the decider's")
    print("           truth table holds, every planted-bad instrument fails by NAME,")
    print("           a timed-out child is verified dead, and the render shouts")
    print("           STALE / UNATTRIBUTED / STATIC / TARGET-MOVED rather than smoothing.")
    return 0


# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(prog="co-lineage.py", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", nargs="?",
                    choices=["coverage", "postmortem", "list", "measure"])
    ap.add_argument("campaign", nargs="?", help="campaign id (see `list`)")
    ap.add_argument("--all-active", action="store_true",
                    help="measure: every active campaign")
    ap.add_argument("--store", type=Path, default=STORE,
                    help="measurement store override (tests)")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()
    if not args.command:
        ap.print_help()
        return 2

    try:
        camps = load_campaigns()
    except DataError as exc:
        print(f"co-lineage: {exc}", file=sys.stderr)
        return 3

    if args.command == "list":
        closed_dir = CAMPAIGNS_DIR / "closed"
        print(f"{'campaign':<28} {'status':<9} {'bars':<5} {'declared':<11} spec")
        for c in camps:
            print(f"{c.id:<28} {c.status:<9} {len(c.bars):<5} {c.declared:<11} {c.spec}")
        closed = sorted(closed_dir.glob("*.toml")) if closed_dir.is_dir() else []
        print(f"\n{len(closed)} closed campaign file(s) in quality/campaigns/closed/"
              + (f": {', '.join(p.stem for p in closed)}" if closed else ""))
        return 0

    if args.command == "measure":
        if args.all_active:
            targets = [c for c in camps if c.status == "active"]
        else:
            if not args.campaign:
                print("co-lineage: measure <campaign> or --all-active", file=sys.stderr)
                return 2
            targets = [c for c in camps if c.id == args.campaign]
            if not targets:
                print(f"co-lineage: no campaign {args.campaign!r} — known: "
                      f"{', '.join(c.id for c in camps) or '(none)'}", file=sys.stderr)
                return 2
        worst = 0
        for c in targets:
            rc = measure_campaign(c, store=args.store)
            worst = max(worst, rc)
        return worst

    if not args.campaign:
        print("co-lineage: which campaign? (`co-lineage.py list`)", file=sys.stderr)
        return 2
    camp = next((c for c in camps if c.id == args.campaign), None)
    if camp is None:
        print(f"co-lineage: no campaign {args.campaign!r} — known: "
              f"{', '.join(c.id for c in camps) or '(none)'}", file=sys.stderr)
        return 2

    rows, malformed = read_measurements(args.store)
    orders = load_orders()
    if args.command == "coverage":
        drift = read_drift_rows()
        render_coverage(camp, orders, rows, malformed, drift)
    else:
        render_postmortem(camp, orders, rows)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
