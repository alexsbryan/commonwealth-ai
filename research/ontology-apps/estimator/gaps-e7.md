# E7 probe: largest judged gap per run, today's EM with an entity source added

| run | E0 | E5 | E6 | E5L | E6L |
|---|---|---|---|---|---|
| c2-lines-gvc-resolve-alone | 0.408 (proposed_answer) | 0.588 (model_choice) | 0.706 (entity:Location) | 0.450 (model_choice) | 0.462 (model_choice) |
| blind-r2--gvc-resolve-alone | 0.737 (model_choice) | 0.748 (model_choice) | 0.737 (model_choice) | 0.738 (model_choice) | 0.738 (model_choice) |
| c2-lines--ward-tune | could-not-judge | 0.153 (entity) | 0.195 (entity:Organization) | n/a | n/a |
| c2-lines--uv-third | 0.055 (proposed_answer) | 0.055 (proposed_answer) | 0.885 (document_thread) | n/a | n/a |
| c2-lines--gvc | 0.466 (model_choice) | 0.706 (document_date) | 0.764 (document_date) | 0.560 (model_choice) | 0.550 (model_choice) |
| blind-r2--ward-tune | 0.165 (proposed_answer) | 0.262 (entity) | 0.615 (entity:Location) | n/a | n/a |
| blind-r2--uv-third | 0.308 (document_thread) | 0.465 (entity) | 0.709 (proposed_answer) | n/a | n/a |
| blind-r2--gvc | 0.501 (document_date) | 0.534 (document_date) | 0.608 (document_date) | 0.470 (document_date) | 0.381 (document_date) |

Verdict under the pre-registered rule: NOT WORTH (stop fired)
Stop clause: c2-lines-gvc-resolve-alone/proposed_answer: entity 0.596 <= 0.607; c2-lines-gvc-resolve-alone/model_choice: entity 0.401 <= 0.677

## Lookalikes: over each text source's agreed labelled pairs, who separates gold-different from gold-same

| run | scope | text source | agreed (same/diff) | text P | entity P / recall / false left | date P / recall | kind P / recall |
|---|---|---|---|---|---|---|---|
| c2-lines-gvc-resolve-alone | document | proposed_answer | 498 (289/209) | 0.58 | 0.596 / 0.997 / 14 left of 209 | 0.493 / 0.253 | 0.607 / 0.716 |
| c2-lines-gvc-resolve-alone | document | model_choice | 216 (87/129) | 0.403 | 0.401 / 0.977 / 2 left of 129 | 0.565 / 0.149 | 0.677 / 0.747 |
| c2-lines-gvc-resolve-alone | lines | proposed_answer | 498 (289/209) | 0.58 | 0.806 / 0.087 / 203 left of 209 | 0.493 / 0.253 | 0.607 / 0.716 |
| c2-lines-gvc-resolve-alone | lines | model_choice | 216 (87/129) | 0.403 | 0.1 / 0.034 / 102 left of 129 | 0.565 / 0.149 | 0.677 / 0.747 |
| blind-r2--gvc-resolve-alone | document | proposed_answer | 510 (146/364) | 0.286 | 0.293 / 0.986 / 17 left of 364 | 0.271 / 0.336 | None / 0.0 (unjudged) |
| blind-r2--gvc-resolve-alone | document | model_choice | 502 (125/377) | 0.249 | 0.248 / 0.992 / 1 left of 377 | 0.105 / 0.016 (unjudged) | None / 0.0 (unjudged) |
| blind-r2--gvc-resolve-alone | lines | proposed_answer | 510 (146/364) | 0.286 | 0.333 / 0.062 / 346 left of 364 | 0.271 / 0.336 | None / 0.0 (unjudged) |
| blind-r2--gvc-resolve-alone | lines | model_choice | 502 (125/377) | 0.249 | 0.176 / 0.048 / 349 left of 377 | 0.105 / 0.016 (unjudged) | None / 0.0 (unjudged) |
| c2-lines--ward-tune | document | proposed_answer | 4 (2/2) | 0.5 | 0.5 / 1.0 (unjudged) / 0 left of 2 | None / 0.0 (unjudged) | None / 0.0 (unjudged) |
| c2-lines--ward-tune | document | model_choice | 8 (8/0) | 1.0 | 1.0 / 1.0 (unjudged) / 0 left of 0 | None / 0.0 (unjudged) | None / 0.0 (unjudged) |
| c2-lines--uv-third | document | proposed_answer | 54 (49/5) | 0.907 | 1.0 / 0.02 (unjudged) / 5 left of 5 | 1.0 / 0.102 (unjudged) | None / 0.0 (unjudged) |
| c2-lines--uv-third | document | model_choice | 5 (3/2) | 0.6 | 1.0 / 1.0 (unjudged) / 2 left of 2 | None / 0.0 (unjudged) | None / 0.0 (unjudged) |
| c2-lines--gvc | document | proposed_answer | 9 (5/4) | 0.556 | 0.556 / 1.0 (unjudged) / 0 left of 4 | 0.556 / 1.0 (unjudged) | 0.5 / 0.4 (unjudged) |
| c2-lines--gvc | document | model_choice | 26 (10/16) | 0.385 | 0.4 / 1.0 / 1 left of 16 | 1.0 / 0.3 (unjudged) | 0.5 / 0.3 (unjudged) |
| c2-lines--gvc | lines | proposed_answer | 9 (5/4) | 0.556 | 1.0 / 0.2 (unjudged) / 4 left of 4 | 0.556 / 1.0 (unjudged) | 0.5 / 0.4 (unjudged) |
| c2-lines--gvc | lines | model_choice | 26 (10/16) | 0.385 | 1.0 / 0.3 (unjudged) / 16 left of 16 | 1.0 / 0.3 (unjudged) | 0.5 / 0.3 (unjudged) |
| blind-r2--ward-tune | document | proposed_answer | 21 (13/8) | 0.619 | 0.619 / 1.0 / 0 left of 8 | None / 0.0 (unjudged) | None / 0.0 (unjudged) |
| blind-r2--ward-tune | document | model_choice | 14 (14/0) | 1.0 | 1.0 / 1.0 (unjudged) / 0 left of 0 | None / 0.0 (unjudged) | None / 0.0 (unjudged) |
| blind-r2--uv-third | document | proposed_answer | 993 (750/243) | 0.755 | 0.909 / 0.12 / 234 left of 243 | 1.0 / 0.008 (unjudged) | None / 0.0 (unjudged) |
| blind-r2--uv-third | document | model_choice | 129 (121/8) | 0.938 | 1.0 / 0.868 / 8 left of 8 | None / 0.0 (unjudged) | None / 0.0 (unjudged) |
| blind-r2--gvc | document | proposed_answer | 2 (1/1) | 0.5 | 0.5 / 1.0 (unjudged) / 0 left of 1 | 0.5 / 1.0 (unjudged) | None / 0.0 (unjudged) |
| blind-r2--gvc | document | model_choice | 39 (18/21) | 0.462 | 0.474 / 1.0 / 1 left of 21 | 0.4 / 0.111 (unjudged) | None / 0.0 (unjudged) |
| blind-r2--gvc | lines | proposed_answer | 2 (1/1) | 0.5 | 1.0 / 1.0 (unjudged) / 1 left of 1 | 0.5 / 1.0 (unjudged) | None / 0.0 (unjudged) |
| blind-r2--gvc | lines | model_choice | 39 (18/21) | 0.462 | 0.75 / 0.167 (unjudged) / 20 left of 21 | 0.4 / 0.111 (unjudged) | None / 0.0 (unjudged) |

## Per source

### c2-lines-gvc-resolve-alone

| source | E0 est (lab, agreed) | E5 est (lab, agreed) | E6 est (lab, agreed) | E5L est (lab, agreed) | E6L est (lab, agreed) |
|---|---|---|---|---|---|
| document_date | 0.155 (0.141, 2368) | 0.311 (0.141, 2368) | 0.755 (0.141, 2368) | 0.174 (0.141, 2368) | 0.19 (0.141, 2368) |
| entity | - | 0.157 (0.124, 9895) | - | 0.258 (0.238, 303) | - |
| entity:Location | - | - | 0.889 (0.183, 6555) | - | 0.338 (0.263, 179) |
| entity:Organization | - | - | 0.643 (0.13, 7853) | - | 0.416 (0.169, 77) |
| entity:Person | - | - | 0.859 (0.168, 5997) | - | 0.429 (0.275, 80) |
| model_choice | 0.753 (0.403, 216) | 0.991 (0.403, 216) | 0.98 (0.403, 216) | 0.852 (0.403, 216) | 0.865 (0.403, 216) |
| necessary:kind | 0.182 (0.218, 3268) | 0.264 (0.218, 3268) | 0.458 (0.218, 3268) | 0.192 (0.218, 3268) | 0.208 (0.218, 3268) |
| proposed_answer | 0.989 (0.58, 498) | 0.995 (0.58, 498) | 0.94 (0.58, 498) | 0.991 (0.58, 498) | 0.991 (0.58, 498) |

### blind-r2--gvc-resolve-alone

| source | E0 est (lab, agreed) | E5 est (lab, agreed) | E6 est (lab, agreed) | E5L est (lab, agreed) | E6L est (lab, agreed) |
|---|---|---|---|---|---|
| document_date | 0.816 (0.237, 916) | 0.804 (0.237, 916) | 0.912 (0.237, 916) | 0.831 (0.237, 916) | 0.857 (0.237, 916) |
| entity | - | 0.207 (0.074, 7308) | - | 0.599 (0.259, 112) | - |
| entity:Location | - | - | 0.732 (0.164, 3241) | - | 0.785 (0.338, 77) |
| entity:Organization | - | - | 0.329 (0.075, 6219) | - | 0.311 (0.032, 31) |
| entity:Person | - | - | 0.732 (0.178, 2696) | - | 0.706 (0.3, 10) unjudged |
| model_choice | 0.986 (0.249, 502) | 0.997 (0.249, 502) | 0.986 (0.249, 502) | 0.987 (0.249, 502) | 0.987 (0.249, 502) |
| proposed_answer | 0.773 (0.286, 510) | 0.859 (0.286, 510) | 0.935 (0.286, 510) | 0.8 (0.286, 510) | 0.819 (0.286, 510) |

### c2-lines--ward-tune

| source | E0 est (lab, agreed) | E5 est (lab, agreed) | E6 est (lab, agreed) |
|---|---|---|---|
| document_date | 0.665 (None, 0) unjudged | 0.665 (None, 0) unjudged | 0.682 (None, 0) unjudged |
| document_id | 0.665 (None, 0) unjudged | 0.665 (None, 0) unjudged | 0.682 (None, 0) unjudged |
| document_thread | 0.665 (None, 0) unjudged | 0.665 (None, 0) unjudged | 0.682 (None, 0) unjudged |
| entity | - | 0.06 (0.213, 122) | - |
| entity:Location | - | - | 0.174 (0.32, 25) |
| entity:Organization | - | - | 0.153 (0.348, 66) |
| entity:Person | - | - | 0.101 (0.234, 111) |
| model_choice | 0.62 (1.0, 8) unjudged | 0.628 (1.0, 8) unjudged | 0.944 (1.0, 8) unjudged |
| necessary:term | 0.116 (0.5, 4) unjudged | 0.117 (0.5, 4) unjudged | 0.154 (0.5, 4) unjudged |
| proposed_answer | 0.729 (0.5, 4) unjudged | 0.731 (0.5, 4) unjudged | 0.839 (0.5, 4) unjudged |

### c2-lines--uv-third

| source | E0 est (lab, agreed) | E5 est (lab, agreed) | E6 est (lab, agreed) |
|---|---|---|---|
| document_date | 0.884 (1.0, 6) unjudged | 0.884 (1.0, 6) unjudged | 0.21 (1.0, 6) unjudged |
| document_id | 0.493 (None, 0) unjudged | 0.493 (None, 0) unjudged | 0.639 (None, 0) unjudged |
| document_thread | 0.978 (0.927, 55) | 0.978 (0.927, 55) | 0.042 (0.927, 55) |
| entity | - | 0.23 (0.421, 19) unjudged | - |
| entity:Location | - | - | 0.595 (0.75, 4) unjudged |
| entity:Organization | - | - | 0.225 (0.471, 17) unjudged |
| entity:Person | - | - | 0.112 (0.667, 3) unjudged |
| model_choice | 0.593 (0.6, 5) unjudged | 0.593 (0.6, 5) unjudged | 0.345 (0.6, 5) unjudged |
| proposed_answer | 0.963 (0.907, 54) | 0.963 (0.907, 54) | 0.024 (0.907, 54) |

### c2-lines--gvc

| source | E0 est (lab, agreed) | E5 est (lab, agreed) | E6 est (lab, agreed) | E5L est (lab, agreed) | E6L est (lab, agreed) |
|---|---|---|---|---|---|
| document_date | 0.618 (0.157, 115) | 0.862 (0.157, 115) | 0.92 (0.157, 115) | 0.617 (0.157, 115) | 0.582 (0.157, 115) |
| document_id | 0.57 (None, 0) unjudged | 0.559 (None, 0) unjudged | 0.626 (None, 0) unjudged | 0.572 (None, 0) unjudged | 0.576 (None, 0) unjudged |
| entity | - | 0.216 (0.043, 952) | - | 0.485 (0.2, 25) | - |
| entity:Location | - | - | 0.757 (0.105, 389) | - | 0.636 (0.231, 13) unjudged |
| entity:Organization | - | - | 0.488 (0.047, 700) | - | 0.401 (0.273, 11) unjudged |
| entity:Person | - | - | 0.714 (0.091, 386) | - | 0.497 (0.0, 3) unjudged |
| model_choice | 0.85 (0.385, 26) | 0.974 (0.385, 26) | 0.985 (0.385, 26) | 0.945 (0.385, 26) | 0.935 (0.385, 26) |
| necessary:kind | 0.104 (0.031, 510) | 0.158 (0.031, 510) | 0.324 (0.031, 510) | 0.117 (0.031, 510) | 0.112 (0.031, 510) |
| proposed_answer | 0.964 (0.556, 9) unjudged | 0.967 (0.556, 9) unjudged | 0.97 (0.556, 9) unjudged | 0.963 (0.556, 9) unjudged | 0.963 (0.556, 9) unjudged |

### blind-r2--ward-tune

| source | E0 est (lab, agreed) | E5 est (lab, agreed) | E6 est (lab, agreed) |
|---|---|---|---|
| document_date | 0.507 (None, 0) unjudged | 0.514 (None, 0) unjudged | 0.691 (None, 0) unjudged |
| document_id | 0.507 (None, 0) unjudged | 0.514 (None, 0) unjudged | 0.691 (None, 0) unjudged |
| document_thread | 0.507 (None, 0) unjudged | 0.514 (None, 0) unjudged | 0.691 (None, 0) unjudged |
| entity | - | 0.099 (0.361, 438) | - |
| entity:Location | - | - | 0.995 (0.38, 187) |
| entity:Organization | - | - | 0.8 (0.419, 246) |
| entity:Person | - | - | 0.744 (0.374, 422) |
| model_choice | 0.513 (1.0, 14) unjudged | 0.636 (1.0, 14) unjudged | 0.98 (1.0, 14) unjudged |
| necessary:direction | 0.101 (0.248, 145) | 0.144 (0.248, 145) | 0.777 (0.248, 145) |
| proposed_answer | 0.784 (0.619, 21) | 0.853 (0.619, 21) | 0.969 (0.619, 21) |

### blind-r2--uv-third

| source | E0 est (lab, agreed) | E5 est (lab, agreed) | E6 est (lab, agreed) |
|---|---|---|---|
| document_date | 0.442 (0.733, 15) unjudged | 0.443 (0.733, 15) unjudged | 0.314 (0.733, 15) unjudged |
| document_id | 0.497 (None, 0) unjudged | 0.499 (None, 0) unjudged | 0.784 (None, 0) unjudged |
| document_thread | 0.995 (0.687, 1345) | 0.995 (0.687, 1345) | 0.112 (0.687, 1345) |
| entity | - | 0.442 (0.907, 482) | - |
| entity:Location | - | - | 0.957 (1.0, 80) |
| entity:Organization | - | - | 0.371 (0.926, 391) |
| entity:Person | - | - | 0.25 (0.852, 108) |
| model_choice | 0.968 (0.938, 129) | 0.973 (0.938, 129) | 0.724 (0.938, 129) |
| proposed_answer | 0.998 (0.755, 993) | 0.998 (0.755, 993) | 0.046 (0.755, 993) |

### blind-r2--gvc

| source | E0 est (lab, agreed) | E5 est (lab, agreed) | E6 est (lab, agreed) | E5L est (lab, agreed) | E6L est (lab, agreed) |
|---|---|---|---|---|---|
| document_date | 0.701 (0.2, 40) | 0.734 (0.2, 40) | 0.808 (0.2, 40) | 0.67 (0.2, 40) | 0.581 (0.2, 40) |
| document_id | 0.619 (None, 0) unjudged | 0.635 (None, 0) unjudged | 0.634 (None, 0) unjudged | 0.607 (None, 0) unjudged | 0.598 (None, 0) unjudged |
| entity | - | 0.361 (0.121, 239) | - | 0.68 (0.545, 11) unjudged | - |
| entity:Location | - | - | 0.68 (0.236, 123) | - | 0.819 (0.625, 8) unjudged |
| entity:Organization | - | - | 0.441 (0.126, 199) | - | 0.446 (0.4, 5) unjudged |
| entity:Person | - | - | 0.865 (0.28, 75) | - | 0.66 (None, 0) unjudged |
| model_choice | 0.641 (0.462, 39) | 0.968 (0.462, 39) | 0.956 (0.462, 39) | 0.609 (0.462, 39) | 0.514 (0.462, 39) |
| proposed_answer | 0.846 (0.5, 2) unjudged | 0.886 (0.5, 2) unjudged | 0.907 (0.5, 2) unjudged | 0.831 (0.5, 2) unjudged | 0.812 (0.5, 2) unjudged |

