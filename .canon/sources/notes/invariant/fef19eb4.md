# MIXED EMBEDDING SPACES FAIL SILENTLY — THEY ABSTAIN ON EVERYTHING AND LOOK LIKE A PASSING TEST. 2026-08-04.

MIXED EMBEDDING SPACES FAIL SILENTLY — THEY ABSTAIN ON EVERYTHING AND LOOK LIKE A PASSING TEST. 2026-08-04.

WHAT HAPPENED. `tests/archive_axis_live.rs` built its class centroids through the classifier's own path but embedded the QUERIES with `inference.embed_query(q)`. When the archive axis briefly moved to the classifier space, the centroids moved and that line did not. Result: 0/6 positives fire, 0/21 negatives fire, "positives firing: 0/6 · false positives: 0/21" — a calm table of zeros that reads like a conservative gate rather than a broken measurement. It was very nearly believed.

WHY IT IS INVISIBLE. Cosine between two different instruction spaces is still a well-formed float in [-1,1]. Nothing errors. Nothing is empty. Every gate simply abstains, which on an axis whose documented posture is "tune for precision, abstains are cheap" is the exact shape of a healthy result.

THREE STRUCTURAL DEFENCES NOW IN PLACE:
1. `router_instruction::axis_space` is the ONE decider mapping axis -> space; the classifiers, the cache freshness gate's `exemplar_specs`, and `router fit` all read it. `router fit` no longer re-derives it (it used to branch `if c.axis == "effort"`), and an unknown axis returns None -> skip + report, never a defaulted space.
2. The router-embed cache folds the instruction text into the `c:` key hash (`router_embed_cache::key`). `built_for` fingerprints the embed MODEL and structurally cannot see an instruction change, so without this a changed instruction leaves every key identical and the freshness gate reports FRESH over vectors from the old space. Watched fail: the gate reported 331/393 missing on the switch.
3. Live axis tests now print sim/margin on ABSTAIN as well as on fire. An abstain with no numbers is the one row you cannot calibrate from — and it is the row that hides this bug.

RULE: whenever you change which space a classifier embeds in, grep every call site that embeds a QUERY for that axis (tests and benches included), not just the centroid/exemplar path. The two are usually in different files.
