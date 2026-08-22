#!/usr/bin/env python3
# -*- coding: utf-8 -*-

import os
import json
import csv
import argparse
def compute_dimension_averages(payload):
    """
    Compute per-dimension averages for three-way scores and the blocked_rate.

    We treat the score as:
        - 1  -> counted as "correct"
        - 0  -> ignored
        - -1 -> counted as "blocked"

    The expected payload structure:
        {
            "task": "...",
            "scores": {
                "info_recall": {
                    "rubric_item_1": {"score": 1, "reason": "...", "evidence": "..."},
                    "rubric_item_2": {"score": 0, "reason": "...", "evidence": "..."},
                    ...
                },
                "analysis": {...},
                "presentation": {...}
            },
            "total_tokens": 1234
        }
    """
    if not isinstance(payload, dict):
        return {}

    out = {}
    scores = payload.get("scores", {}) or {}
    
    # Mapping: JSON key -> output dimension name
    dim_map = [("info_recall", "inforecall"), ("analysis", "analysis"), ("presentation", "presentation")]

    total_ones = 0
    total_minus_ones = 0
    total_items = 0

    for json_key, out_key in dim_map:
        dim_obj = scores.get(json_key)
        if isinstance(dim_obj, dict) and dim_obj:
            # Collect all score values for this dimension
            score_values = []
            for item_key, item_val in dim_obj.items():
                if isinstance(item_val, dict):
                    score = item_val.get("score")
                    if isinstance(score, (int, float)):
                        score_values.append(score)
            
            # Compute the fraction of items with score == 1
            ones_count = sum(1 for s in score_values if s == 1)
            minus_ones_count = sum(1 for s in score_values if s == -1)
            items_count = len(score_values)
            
            if items_count > 0:
                out[out_key] = ones_count / items_count
            else:
                out[out_key] = None
            
            total_ones += ones_count
            total_minus_ones += minus_ones_count
            total_items += items_count
        else:
            out[out_key] = None

    # Compute overall score (fraction of items with score == 1 across all dimensions)
    if total_items > 0:
        out["total"] = total_ones / total_items
        out["blocked_rate"] = total_minus_ones / total_items
    else:
        out["total"] = None
        out["blocked_rate"] = None

    return out
