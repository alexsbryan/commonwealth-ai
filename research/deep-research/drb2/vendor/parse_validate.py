# -*- coding: utf-8 -*-

import os
import re
import json
FENCED_JSON_PATTERN = r'```json\s*(.*)```'

def _try_clean_and_load(s: str):
    json_clean = re.sub(
        r'"(?P<k>.*?)"(?=\s*:)',
        lambda m: '"' + re.sub(r'(?<!\\)"', r'\"', m.group('k')) + '"',
        s
    )
    return json.loads(json_clean.strip())

def parse_model_text(text: str):
    matches = re.findall(FENCED_JSON_PATTERN, text, re.DOTALL)
    if matches:
        try:
            return _try_clean_and_load(matches[0]), True
        except json.JSONDecodeError as e:
            print(f"[warn] failed to parse fenced JSON: {e}; trying full text...")
    try:
        return _try_clean_and_load(text), True
    except json.JSONDecodeError as e:
        print(f"[warn] failed to parse JSON from full text: {e}")
        return None, False

# =========================
# Batched evaluation and validation
# =========================
def validate_batch_result(rubric_items: List[str], parsed_result: Dict) -> bool:
    """
    Validate that the model output contains all rubric_items with exact text match.
    """
    if not isinstance(parsed_result, dict):
        return False
    results = parsed_result.get("results", [])
    if not isinstance(results, list):
        return False
    if len(results) != len(rubric_items):
        return False
    
    # 检查每个 rubric_item 是否严格匹配
    returned_items = [r.get("rubric_item", "") for r in results]
    for expected in rubric_items:
        # Ensure every rubric_item from the input is present in the results
        if expected not in returned_items:
            return False
    
    return True
