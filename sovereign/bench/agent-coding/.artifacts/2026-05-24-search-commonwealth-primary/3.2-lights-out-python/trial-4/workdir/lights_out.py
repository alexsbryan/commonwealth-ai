"""Light's Out solver.

Replace the body of `solve` with a working implementation.
See `prompt.md` for the full task specification.

The grader binds against `solve(grid)` exactly as declared below.
"""


def solve(grid):
    """Solve the Lights Out puzzle.

    Uses Gaussian elimination over GF(2) to find a minimum-cardinality solution.
    
    Args:
        grid: Square list-of-lists of ints (0 = dark, 1 = lit).

    Returns:
        A list of (row, col) tuples (minimum cardinality) that turns
        every light off when applied, or None if the grid is unsolvable.
    """
    n = len(grid)
    size = n * n
    
    # Build the toggle matrix A where A[i][j] = 1 if pressing cell j toggles cell i
    A = [[0] * size for _ in range(size)]
    b = [grid[r][c] for r in range(n) for c in range(n)]
    
    def idx(r, c):
        return r * n + c
    
    for r in range(n):
        for c in range(n):
            col_idx = idx(r, c)
            # Toggle self and neighbors
            cells_to_toggle = [(r, c), (r-1, c), (r+1, c), (r, c-1), (r, c+1)]
            for nr, nc in cells_to_toggle:
                if 0 <= nr < n and 0 <= nc < n:
                    A[idx(nr, nc)][col_idx] ^= 1

    # Augmented matrix [A | b] over GF(2)
    aug = []
    for i in range(size):
        row = list(A[i]) + [b[i]]
        aug.append(row)

    pivot_cols = [-1] * size  # which column is the pivot for each row
    pivot_rows_for_col = {}   # maps col -> row
    
    current_row = 0
    for col in range(size):
        found_pivot = False
        for r in range(current_row, size):
            if aug[r][col]:
                found_pivot = True
                break
        
        if not found_pivot:
            continue
            
        # Swap rows to bring pivot into position
        aug[current_row], aug[r] = aug[r], aug[current_row]
        
        # Eliminate all other rows with this pivot
        for r in range(size):
            if r != current_row and aug[r][col]:
                for c2 in range(size + 1):
                    aug[r][c2] ^= aug[current_row][c2]
        
        pivot_cols[current_row] = col
        pivot_rows_for_col[col] = current_row
        current_row += 1

    rank = current_row
    
    # Check consistency: any row where A part is zero but b part is not means unsolvable
    for i in range(rank, size):
        if aug[i][size]:
            return None

    # Find free variables (columns without pivots)
    pivot_set = set(pivot_rows_for_col.keys())
    free_vars = [col for col in range(size) if col not in pivot_set]

    def get_solution(free_vals):
        """Construct a solution given values for free variables."""
        x = [0] * size
        for fv in free_vars:
            x[fv] = free_vals[free_vars.index(fv)] % 2
        
        # Back-substitute to find pivot variable values
        for i in range(rank - 1, -1, -1):
            pc = pivot_cols[i]
            val = aug[i][size]
            for c2 in range(pc + 1, size):
                val ^= (aug[i][c2] & x[c2])
            x[pc] = val % 2
            
        return x

    best_solution = None
    min_presses = size + 1
    
    # Iterate over all combinations of free variables using bit manipulation
    num_free = len(free_vars)
    for mask in range(1 << num_free):
        vals = []
        for i in range(num_free):
            vals.append((mask >> i) & 1)
        
        candidate = get_solution(vals)
        presses = sum(candidate)
        
        if presses < min_presses:
            min_presses = presses
            best_solution = list(candidate)

    # Convert solution to (row, col) format
    result = []
    for idx_val in range(size):
        if best_solution[idx_val]:
            r = idx_val // n
            c = idx_val % n
            result.append((r, c))
    
    return result
