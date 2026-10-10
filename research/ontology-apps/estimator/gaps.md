# Estimator candidates: largest judged gap per run (|estimated − labelled| on sources with >= 20 agreed labelled pairs)

| run | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|
| c2-lines-gvc-resolve-alone | 0.408 (proposed_answer) | 0.828 (document_date) | 0.528 (document_date) | 0.828 (document_date) | 0.216 (model_choice) | 0.413 (proposed_answer) | 0.184 (model_choice) |
| blind-r2--gvc-resolve-alone | 0.737 (model_choice) | 0.740 (model_choice) | 0.716 (document_date) | 0.680 (document_date) | 0.252 (model_choice) | 0.348 (proposed_answer) | 0.226 (model_choice) |
| c2-lines--ward-tune | could-not-judge | could-not-judge | could-not-judge | could-not-judge | could-not-judge | could-not-judge | could-not-judge |
| c2-lines--uv-third | 0.055 (proposed_answer) | 0.061 (document_thread) | 0.053 (document_thread) | 0.060 (document_thread) | 0.012 (proposed_answer) | 0.066 (proposed_answer) | 0.011 (proposed_answer) |
| c2-lines--gvc | 0.466 (model_choice) | 0.785 (document_date) | 0.679 (document_date) | 0.776 (document_date) | 0.499 (model_choice) | 0.050 (document_date) | 0.302 (model_choice) |
| blind-r2--ward-tune | 0.165 (proposed_answer) | 0.163 (proposed_answer) | 0.095 (proposed_answer) | 0.095 (proposed_answer) | 0.381 (proposed_answer) | 0.289 (proposed_answer) | 0.125 (proposed_answer) |
| blind-r2--uv-third | 0.308 (document_thread) | 0.243 (proposed_answer) | 0.855 (model_choice) | 0.855 (model_choice) | 0.043 (model_choice) | 0.303 (document_thread) | 0.073 (document_thread) |
| blind-r2--gvc | 0.501 (document_date) | 0.754 (document_date) | 0.556 (document_date) | 0.747 (document_date) | 0.443 (document_date) | 0.216 (document_date) | 0.260 (document_date) |

Chosen under the pre-registered rule: none (E0 stays)

## E1: is a document field's random-pair u the u the weighed pairs have?

| run | source | random-pair u | labelled u (proposed pairs) |
|---|---|---|---|
| c2-lines-gvc-resolve-alone | document_date | 0.0097 | 0.1784 |
| c2-lines-gvc-resolve-alone | necessary:kind | 0.1768 | 0.2124 |
| blind-r2--gvc-resolve-alone | document_date | 0.0097 | 0.077 |
| c2-lines--ward-tune | document_date | 0.0 | 0.0 |
| c2-lines--ward-tune | document_thread | 0.0 | 0.0 |
| c2-lines--uv-third | document_date | 0.002 | 0.0 |
| c2-lines--uv-third | document_thread | 0.0445 | 0.0263 |
| c2-lines--gvc | document_date | 0.0097 | 0.0623 |
| blind-r2--ward-tune | document_date | 0.0 | 0.0 |
| blind-r2--ward-tune | document_thread | 0.0 | 0.0 |
| blind-r2--uv-third | document_date | 0.0001 | 0.0015 |
| blind-r2--uv-third | document_thread | 0.1149 | 0.1608 |
| blind-r2--gvc | document_date | 0.0097 | 0.0741 |

## Per source

### c2-lines-gvc-resolve-alone (labelled base rate 0.0867)

| source | agreed | labelled [CI90] | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|---|---|
| document_date | 2368 | 0.141 [0.091, 0.188] | 0.155 | 0.969 | 0.669 | 0.97 | 0.127 | 0.218 | 0.197 |
| model_choice | 216 | 0.403 [0.349, 0.455] | 0.753 | 0.99 | 0.911 | 0.941 | 0.618 | 0.765 | 0.586 |
| necessary:kind | 3268 | 0.218 [0.192, 0.244] | 0.182 | 0.532 | 0.351 | 0.537 | 0.169 | 0.263 | 0.275 |
| proposed_answer | 498 | 0.58 [0.528, 0.638] | 0.989 | 0.994 | 0.984 | 0.987 | 0.625 | 0.993 | 0.673 |

### blind-r2--gvc-resolve-alone (labelled base rate 0.0535)

| source | agreed | labelled [CI90] | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|---|---|
| document_date | 916 | 0.237 [0.161, 0.283] | 0.816 | 0.93 | 0.953 | 0.917 | 0.379 | 0.388 | 0.354 |
| model_choice | 502 | 0.249 [0.204, 0.292] | 0.986 | 0.989 | 0.147 | 0.144 | 0.501 | 0.569 | 0.475 |
| proposed_answer | 510 | 0.286 [0.245, 0.325] | 0.773 | 0.814 | 0.565 | 0.564 | 0.453 | 0.635 | 0.44 |

### c2-lines--ward-tune (labelled base rate 0.2114)

| source | agreed | labelled [CI90] | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|---|---|
| document_date | 0 | none agreed | 0.665 (unjudged) | 1.0 (unjudged) | 0.45 (unjudged) | 0.999 (unjudged) | 1.0 (unjudged) | 0.581 (unjudged) | 1.0 (unjudged) |
| document_id | 0 | none agreed | 0.665 (unjudged) | 1.0 (unjudged) | 0.45 (unjudged) | 0.999 (unjudged) | 1.0 (unjudged) | 0.581 (unjudged) | 1.0 (unjudged) |
| document_thread | 0 | none agreed | 0.665 (unjudged) | 1.0 (unjudged) | 0.45 (unjudged) | 0.999 (unjudged) | 1.0 (unjudged) | 0.581 (unjudged) | 1.0 (unjudged) |
| model_choice | 8 | 1.0 [1.0, 1.0] | 0.62 (unjudged) | 0.618 (unjudged) | 0.03 (unjudged) | 0.03 (unjudged) | 1.0 (unjudged) | 0.891 (unjudged) | 1.0 (unjudged) |
| necessary:term | 4 | 0.5 [0.0, 1.0] | 0.116 (unjudged) | 0.115 (unjudged) | 0.007 (unjudged) | 0.007 (unjudged) | 0.599 (unjudged) | 0.365 (unjudged) | 0.54 (unjudged) |
| proposed_answer | 4 | 0.5 [0.0, 1.0] | 0.729 (unjudged) | 0.728 (unjudged) | 0.058 (unjudged) | 0.058 (unjudged) | 0.637 (unjudged) | 0.894 (unjudged) | 0.619 (unjudged) |

### c2-lines--uv-third (labelled base rate 0.2796)

| source | agreed | labelled [CI90] | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|---|---|
| document_date | 6 | 1.0 [1.0, 1.0] | 0.884 (unjudged) | 0.947 (unjudged) | 0.886 (unjudged) | 0.947 (unjudged) | 1.0 (unjudged) | 0.908 (unjudged) | 1.0 (unjudged) |
| document_id | 0 | none agreed | 0.493 (unjudged) | 0.492 (unjudged) | 0.495 (unjudged) | 0.495 (unjudged) | 1.0 (unjudged) | 0.555 (unjudged) | 1.0 (unjudged) |
| document_thread | 55 | 0.927 [0.875, 0.982] | 0.978 | 0.866 | 0.98 | 0.868 | 0.917 | 0.984 | 0.935 |
| model_choice | 5 | 0.6 [0.0, 1.0] | 0.593 (unjudged) | 0.583 (unjudged) | 0.239 (unjudged) | 0.239 (unjudged) | 0.795 (unjudged) | 0.648 (unjudged) | 0.843 (unjudged) |
| proposed_answer | 54 | 0.907 [0.849, 0.965] | 0.963 | 0.961 | 0.939 | 0.937 | 0.896 | 0.974 | 0.918 |

### c2-lines--gvc (labelled base rate 0.0257)

| source | agreed | labelled [CI90] | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|---|---|
| document_date | 115 | 0.157 [0.091, 0.222] | 0.618 | 0.941 | 0.836 | 0.932 | 0.605 | 0.207 | 0.293 |
| document_id | 0 | none agreed | 0.57 (unjudged) | 0.544 (unjudged) | 0.506 (unjudged) | 0.505 (unjudged) | 0.998 (unjudged) | 0.478 (unjudged) | 0.996 (unjudged) |
| model_choice | 26 | 0.385 [0.219, 0.56] | 0.85 | 0.854 | 0.389 | 0.394 | 0.883 | 0.395 | 0.687 |
| necessary:kind | 510 | 0.031 [0.019, 0.045] | 0.104 | 0.142 | 0.113 | 0.121 | 0.109 | 0.034 | 0.034 |
| proposed_answer | 9 | 0.556 [0.429, 1.0] | 0.964 (unjudged) | 0.965 (unjudged) | 0.964 (unjudged) | 0.965 (unjudged) | 0.828 (unjudged) | 0.942 (unjudged) | 0.673 (unjudged) |

### blind-r2--ward-tune (labelled base rate 0.3511)

| source | agreed | labelled [CI90] | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|---|---|
| document_date | 0 | none agreed | 0.507 (unjudged) | 0.999 (unjudged) | 0.481 (unjudged) | 0.998 (unjudged) | 0.999 (unjudged) | 0.506 (unjudged) | 0.999 (unjudged) |
| document_id | 0 | none agreed | 0.507 (unjudged) | 0.999 (unjudged) | 0.481 (unjudged) | 0.998 (unjudged) | 0.999 (unjudged) | 0.506 (unjudged) | 0.999 (unjudged) |
| document_thread | 0 | none agreed | 0.507 (unjudged) | 0.999 (unjudged) | 0.481 (unjudged) | 0.998 (unjudged) | 0.999 (unjudged) | 0.506 (unjudged) | 0.999 (unjudged) |
| model_choice | 14 | 1.0 [1.0, 1.0] | 0.513 (unjudged) | 0.511 (unjudged) | 0.459 (unjudged) | 0.459 (unjudged) | 1.0 (unjudged) | 0.739 (unjudged) | 1.0 (unjudged) |
| necessary:direction | 145 | 0.248 [0.1, 0.45] | 0.101 | 0.1 | 0.191 | 0.189 | 0.047 | 0.527 | 0.351 |
| proposed_answer | 21 | 0.619 [0.273, 0.963] | 0.784 | 0.782 | 0.714 | 0.714 | 0.238 | 0.908 | 0.494 |

### blind-r2--uv-third (labelled base rate 0.3381)

| source | agreed | labelled [CI90] | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|---|---|
| document_date | 15 | 0.733 [0.533, 0.909] | 0.442 (unjudged) | 0.97 (unjudged) | 0.416 (unjudged) | 0.968 (unjudged) | 0.66 (unjudged) | 0.421 (unjudged) | 0.7 (unjudged) |
| document_id | 0 | none agreed | 0.497 (unjudged) | 0.494 (unjudged) | 0.471 (unjudged) | 0.473 (unjudged) | 0.997 (unjudged) | 0.476 (unjudged) | 0.998 (unjudged) |
| document_thread | 1345 | 0.687 [0.588, 0.783] | 0.995 | 0.789 | 0.998 | 0.783 | 0.72 | 0.99 | 0.76 |
| model_choice | 129 | 0.938 [0.875, 0.969] | 0.968 | 0.914 | 0.083 | 0.083 | 0.981 | 0.962 | 0.984 |
| proposed_answer | 993 | 0.755 [0.693, 0.811] | 0.998 | 0.998 | 0.996 | 0.996 | 0.781 | 0.998 | 0.811 |

### blind-r2--gvc (labelled base rate 0.0644)

| source | agreed | labelled [CI90] | E0 | E2 | E3 | E4 | O1 | O2 | O3 |
|---|---|---|---|---|---|---|---|---|---|
| document_date | 40 | 0.2 [0.077, 0.368] | 0.701 | 0.954 | 0.756 | 0.947 | 0.643 | 0.416 | 0.46 |
| document_id | 0 | none agreed | 0.619 (unjudged) | 0.585 (unjudged) | 0.557 (unjudged) | 0.546 (unjudged) | 1.0 (unjudged) | 0.565 (unjudged) | 0.999 (unjudged) |
| model_choice | 39 | 0.462 [0.326, 0.6] | 0.641 | 0.671 | 0.372 | 0.401 | 0.728 | 0.365 | 0.537 |
| proposed_answer | 2 | 0.5 [0.0, 1.0] | 0.846 (unjudged) | 0.852 (unjudged) | 0.74 (unjudged) | 0.747 (unjudged) | 0.876 (unjudged) | 0.785 (unjudged) | 0.804 (unjudged) |

