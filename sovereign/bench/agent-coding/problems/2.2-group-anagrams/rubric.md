# 2.2 group-anagrams — judge rubric anchors

## dim_b

### 0
No runnable function, or O(N²) all-pairs anagram check, or O(N·N!)
attempt to enumerate permutations. Doesn't normalize the key.

### 1
HashMap-keyed approach with bugs: doesn't preserve insertion order
(uses HashMap with arbitrary iteration → groups order non-deterministic),
or returns groups in HashMap-iteration order which fails the ordering
tests.

### 2
Correct sorted-key HashMap approach with explicit order preservation:
either uses IndexMap, or maintains an insertion-order Vec of keys
alongside the HashMap. All twelve fixture tests pass.

### 3
Clean implementation: sorts each string's chars to make the key,
uses `HashMap<String, Vec<String>>` plus a `Vec<String>` of
first-seen keys for order preservation. Or an IndexMap if the
candidate knows it. ≤ 15 lines body.

## dim_c

### 0
Incoherent, panics on happy path, doesn't compile.

### 1
Compiles but noisy: redundant clones, casts, mutable state that
could be immutable.

### 2
Idiomatic: uses `entry().or_insert_with()` cleanly, single sort per
key, no unnecessary cloning.

### 3
Minimal and idiomatic. The implementation reads like a textbook
example. No noise.
