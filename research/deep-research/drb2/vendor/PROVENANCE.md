# Vendored files — provenance and integrity

All four files are BYTE-EXACT copies from the pinned DRB-II clone:

- Repo: `imlrz/DeepResearch-Bench-II`
- Commit: `087c1b8d4a0ed46fd3dd8615a0b5e93ce3acf6f8` (cloned 2026-08-19, read-only)
- Source tree: `/home/alexbryan/dev/DeepResearch-Bench-II` (a clone of that commit)
- License: Apache License 2.0 (code), per the benchmark's LICENSE

| Vendored file | Source file (pinned clone) | Lines |
|---|---|---|
| `prompt_template.py` | `run_evaluation.py` | 69-125 (PROMPT_TEMPLATE) |
| `parse_validate.py` | `run_evaluation.py` | 1-4 (docstring + imports), 230-275 (FENCED_JSON_PATTERN, _try_clean_and_load, parse_model_text, validate_batch_result) |
| `aggregation.py` | `aggregate_scores.py` | 1-7 (docstring + imports), 32-104 (compute_dimension_averages) |
| `gpt_client.py` | `gpt_client.py` | 1-138 (complete file) |

Verification (run from this directory):

```
REPO=/home/alexbryan/dev/DeepResearch-Bench-II
sed -n '69,125p' "$REPO/run_evaluation.py" | cmp - prompt_template.py
{ sed -n '1,4p' "$REPO/run_evaluation.py"; sed -n '230,275p' "$REPO/run_evaluation.py"; } | cmp - parse_validate.py
{ sed -n '1,7p' "$REPO/aggregate_scores.py"; sed -n '32,104p' "$REPO/aggregate_scores.py"; } | cmp - aggregation.py
cmp "$REPO/gpt_client.py" gpt_client.py
```

SHA256SUMS hashes the four files as they sit in this directory. The files
are never edited in place; if the protocol ever changes upstream, the new
pinned clone is vendored into a new directory and this file is rewritten
with the new provenance, never merged silently.

The scorer (`../drb2-score.py`) is the ONLY executable consumer of these
files; it imports them directly (one implementation — §10.6).
