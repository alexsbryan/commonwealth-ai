# RUN MANIFEST — converge-s2 (CONVERGE Session 2)

| field | value |
|---|---|
| script | run.sh (plist converge-s2.plist, label com.svrn.seat.converge-s2, bootstrapped from this directory). Six legs, one at a time: ours/uv-third, ours/ward-tune, ours/gvc, blind/gvc, blind/ward-tune, blind/uv-third |
| binary | bin/svrn-ingest, target/debug/svrn-ingest built -p sovereign-pipeline after d256c8d72 (no code change since); HEAD d1fee1c15 at staging, tracked tree clean (bin/tree-at-staging.txt empty); sha256 8d006ffbef01c25c |
| what this binary adds over the baseline (2ec98c0237ffb239 at 18456fd9f) | cf33c151d line classes; 7e043fce8 subject readings on claims, read contract v6; 3f7148016 one locator; d256c8d72 record folds (by) |
| ours | job-ours.sh = build-step9's with R and BIN changed; the TRACKED recipes at d1fee1c15: support/recipe.toml (695b17d2841f39ab), ward/recipe.toml (8602e09b30a2d3eb), cdcr/recipe-gvc-e2e.toml (bb79d11a67345170); populations as build-step9 |
| blind | job-blind.sh = build-step9's with R, BIN and sections.py changed; ~/blind-author-20261010-r3/{gvc,ward,uv}/recipe.toml unedited (b7c0d4c61949438a, c912e2bfccdf0512, b91fcf303de26c4e); population/*.docs as build-step9 |
| not run | the two RESOLVE-alone legs (agreement.py reports them never-ran) |
| replay | every job leg, as build-step9: --asker replay against 127.0.0.1:9, 7 record files byte-compared, refusals counted; <leg>/replay.txt |
| reading afterwards | ladder.py --ward ours/ward-tune --uv ours/uv-third --gvc ours/gvc (and blind/); blind-author/agreement.py --blind blind --ours ours --ours-resolve ours-resolve-alone --out agreement |
| expected | ours about 1.5-2.5 h (build-step9's took 83 min; S1.2 asks more lines, Ward's recipe adds Pick, Mention and two Point fields); blind about 3-4 h (blind-r3: GVC 51 min, Ward 93 min, uv over 2 h) |
| daemon | :9741 with commonwealth/primary and qwen-embedding-0.6b; idle for inference at staging (last chat completion 23:32Z); no restart, no config change |
