# Run-length encoding — encode/decode (Python)

Implement classic run-length encoding: compress a string by
replacing consecutive runs of the same character with the run
length followed by the character.

## Your task

Implement in `rle.py` at the workdir root:

```python
def encode(text: str) -> str
def decode(text: str) -> str
```

- `encode("AABBBCCCC") == "2A3B4C"`
- Runs of length 1 are emitted WITHOUT a count: `encode("XYZ") == "XYZ"`.
- Counts may be multi-digit: `encode("A"*12) == "12A"` and
  `decode("12A") == "A"*12`.
- `decode` is the exact inverse: `decode(encode(s)) == s` for any
  string whose characters are letters or spaces (the input never
  contains digits).
- Empty string maps to empty string in both directions.
- Spaces are data like any other character.
- Standard library only. The grader imports `from rle import
  encode, decode` exactly as declared.
