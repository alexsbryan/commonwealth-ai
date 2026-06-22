"""Light's Out solver.

Uses Gaussian elimination over GF(2) to find minimum-cardinality solutions.
The key insight: pressing cell (i,j) toggles it and its orthogonal neighbors,
and the order of presses doesn't matter. We model this as a linear system Ax=b
over GF(2), where A is the adjacency+identity matrix, b is the initial grid
(flattened), and x is the press pattern we want to find.
"""

from itertools import product


def solve(grid):
    """Solve the Lights Out puzzle.

    Args:
        grid: Square list-of-lists of ints (0 = dark, 1 = lit).

    Returns:
        A list of (row, col) tuples (minimum cardinality) that turns
        every light off when applied, or None if the grid is unsolvable.
    """
    n = len(grid)
    N = n * n  # total cells

    def idx(r, c):
        return r * n + c

    # Build toggle matrix rows as bitmasks. Bit j set means pressing cell j toggles cell i.
    A_rows = [0] * N
    for r in range(n):
        for c in range(n):
            i = idx(r, c)
            for dr, dc in ((0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)):
                nr, nc = r + dr, c + dc
                if 0 <= nr < n and 0 <= nc < n:
                    j = idx(nr, nc)
                    A_rows[i] |= (1 << j)

    # b vector as bitmask
    b_mask = 0
    for r in range(n):
        for c in range(n):
            if grid[r][c]:
                b_mask |= (1 << idx(r, c))

    # Augmented matrix: each row is an integer with N+1 bits.
    # Lower N bits are the A-row; bit N is the augmented column (b value).
    aug = [(A_rows[i] << 1) | ((b_mask >> i) & 1) for i in range(N)]

    pivot_col_of_row = [-1] * N   # which col is pivoted at this row index
    col_to_pivot_row = [-1] * N   # which row has a pivot on this col
    num_pivots = 0                # number of rows used so far

    cur = 0  # current row being filled with a pivot
    for col in range(N):
        # Find a candidate row with bit `col` set (among remaining rows)
        found = None
        for row in range(cur, N):
            if (aug[row] >> col) & 1:
                found = row
                break
        if found is None:
            continue  # free variable — no pivot here
        aug[cur], aug[found] = aug[found], aug[cur]
        pivot_col_of_row[cur] = col
        col_to_pivot_row[col] = cur
        num_pivots += 1
        # Eliminate this column from ALL other rows (Gauss-Jordan over GF(2))
        mask = aug[cur]
        for row in range(N):
            if row != cur and ((aug[row] >> col) & 
{write_file_path=/private/var/folders/qf