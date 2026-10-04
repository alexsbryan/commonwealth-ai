Constraints:
- Modify only source files. Do NOT add, edit, or delete test files — the
  grader restores them and your edits there are discarded.
- Do not change dependency pins or config unrelated to the issue.
- The fix is judged by held-out tests you cannot see. Make the smallest
  change that genuinely resolves the reported behaviour.
- Leave the working tree with your fix applied. Do not commit.

You can run the project's real test suite. The dependencies are already
installed in a container image; your working tree is mounted into it, so
edits take effect immediately. Run it with:

    {verify_cmd}

Narrow it to one test while iterating, e.g. append a `::test_name` to the
path. This is the same environment the grader uses.
