<!-- ledger -->

**phase-b-101 · 2026-10-01 · phase-c · operator, consolidating phase-c from 28 rows to 20 with no content dropped** — this commit
- Needed: the operator expected about 20 rows; phase-c held 28 (10 cut-introduced, 8 bugs main shares, 10 cleanup), several of them a few lines each and filed one per finding.
- Chose (operator: "yes, consolidate to 20"): pc-onprem-followups holds pc-onprem-ocr-cleanup-key, pc-cli-client-credential, pc-sealed-posture-web-offer and pc-onprem-absence-messages; pc-split-deploy-honesty holds pc-admin-reload-checks-serve, pc-serve-restart-self-report and pc-mesh-status-serve-down (pc-rpc-probe-identity, a security finding, stays its own row); pc-cli-base-residue absorbs pc-cli-dev-probe-twin, pc-nudge-dismiss-recipe-publish and pc-serving-lift-script. Each merged item keeps its full text as a bullet tagged with its old id, so earlier ledgers' references resolve, and each is judged on its own bullet. The ship gate's live text names the new ids.
- Because: row count was being read as size; the merge changes neither scope nor content.
