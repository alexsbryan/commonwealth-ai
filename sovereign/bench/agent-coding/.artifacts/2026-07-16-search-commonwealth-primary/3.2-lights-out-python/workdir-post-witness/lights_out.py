"""Light's Out solver.

Replace the body of `solve` with a working implementation.
See `prompt.md` for the full task specification.

The grader binds against `solve(grid)` exactly as declared below.
"""


"""Light's Out solver.

Replace the body of `solve` with a working implementation.
See `prompt.md` for the full task specification.

The grader binds against `solve(grid)` exactly as declared below.
"""


def solve(grid):
    """Solve the Lights Out puzzle.

    Args:
        grid: Square list-of-lists of ints (0 = dark, 1 = lit).

    Returns:
        A list of (row, col) tuples (minimum cardinality) that turns
        every light off when applied, or None if the grid is unsolvable.
    """
    n = len(grid)
    
    # Build the toggle matrix and target vector over GF(2)
    size = n * n
    
    # For each cell i (r,c), determine which cells it toggles
    # We'll build an augmented matrix [A | b] where Ax = b in GF(2)
    # A[i][j] = 1 if pressing j affects cell i
    # b[i] = grid[r][c] (we want to cancel this out)
    
    aug = [[0] * (size + 1) for _ in range(size)]
    
    def idx(r, c):
        return r * n + c
    
    for r in range(n):
        for c in range(n):
            j = idx(r, c)  # column: pressing (r,c)
            row_idx = j     # we're building the effect on each target cell
            
            # Pressing (r,c) toggles itself and orthogonal neighbors
            targets = [(r, c), (r-1, c), (r+1, c), (r, c-1), (r, c+1)]
            
            for tr, tc in targets:
                if 0 <= tr < n and 0 <= tc < n:
                    i = idx(tr, tc)
                    aug[i][j] ^= 1
            
            b_val = grid[r][c]
            aug[idx(r, c)][size] = b_val
    
    # Gaussian elimination over GF(2) on the augmented matrix
    pivot_cols = []
    row_idx = 0
    
    for col in range(size):
        # Find a row with a 1 in this column at or below row_idx
        found = -1
        for r in range(row_idx, size):
            if aug[r][col]:
                found = r
                break
        
        if found == -1:
            continue  # free variable
        
        # Swap rows
        aug[row_idx], aug[found] = aug[found], aug[row_idx]
        
        # Eliminate all other rows
        for r in range(size):
            if r != row_idx and aug[r][col]:
                for c2 in range(col, size + 1):
                    aug[r][c2] ^= aug[row_idx][c2]
        
        pivot_cols.append((row_idx, col))
        row_idx += 1
    
    # Check consistency: any row with all zeros on left but non-zero on right means unsolvable
    for r in range(row_idx - 1, size) if False else range(0, size):
        pass
    
    # Actually check from the bottom up after elimination
    for r in range(size):
        if not any(aug[r][c] for c in range(size)):
            if aug[r][size]:
                return None  # inconsistent system
    
    # Extract solution: for each column that is a pivot column, 
    # x[col] = augmented value of its pivot row's last element
    x = [0] * size
    for (prow, pcol) in pivot_cols:
        x[pcol] = aug[prow][size]
    
    # Convert to list of coordinates where press is needed
    result = []
    for i in range(size):
        if x[i]:
            r, c = divmod(i, n)
            result.append((r, c))
    
    return result
