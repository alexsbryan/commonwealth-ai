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
    
    # Convert to a flat bitmask for efficiency
    state = 0
    for r in range(n):
        for c in range(n):
            if grid[r][c]:
                state |= (1 << (r * n + c))
    
    # Precompute toggle masks for each cell press
    toggles = []
    for r in range(n):
        for c in range(n):
            mask = 0
            # Toggle self and neighbors
            positions = [(r, c)]
            if r > 0: positions.append((r-1, c))
            if r < n-1: positions.append((r+1, c))
            if c > 0: positions.append((r, c-1))
            if c < n-1: positions.append((r, c+1))
            
            for pr, pc in positions:
                mask |= (1 << (pr * n + pc))
            toggles.append(mask)
    
    # Gaussian elimination over GF(2) to find all solutions
    # We want to solve A*x = state (mod 2), where A is the toggle matrix
    
    # Build augmented matrix [A | b]
    num_vars = n * n
    rows = []
    for i in range(num_vars):
        row = [0] * (num_vars + 1)
        for j in range(num_vars):
            if toggles[j] & (1 << i):
                row[j] = 1
        if state & (1 << i):
            row[num_vars] = 1
        rows.append(row)
    
    # Gaussian elimination with partial pivoting over GF(2)
    pivot_row = 0
    pivot_cols = []
    for col in range(num_vars):
        found_pivot = False
        for r in range(pivot_row, num_vars):
            if rows[r][col]:
                # Swap rows
                rows[pivot_row], rows[r] = rows[r], rows[pivot_row]
                found_pivot = True
                break
        
        if not found_pivot:
            continue
            
        pivot_cols.append(col)
        
        # Eliminate other rows
        for r in range(num_vars):
            if r != pivot_row and rows[r][col]:
                for c_idx in range(num_vars + 1):
                    rows[r][c_idx] ^= rows[pivot_row][c_idx]
        
        pivot_row += 1
    
    rank = len(pivot_cols)
    
    # Check consistency - any row with all zeros on left but 1 on right means no solution
    for r in range(rank, num_vars):
        if rows[r][num_vars]:
            return None
    
    # Find free variables (columns not in pivot_cols)
    free_vars = [i for i in range(num_vars) if i not in pivot_cols]
    
    # Generate all solutions by trying different combinations of free variables
    min_solution = None
    min_count = float('inf')
    
    num_free = len(free_vars)
    for mask in range(1 << num_free):
        x = [0] * num_vars
        
        # Set free variable values based on current combination
        for idx, fv in enumerate(free_vars):
            x[fv] = (mask >> idx) & 1
        
        # Back-substitute to find dependent variables
        valid = True
        for i, pc in enumerate(pivot_cols):
            val = rows[i][num_vars]
            for j in range(pc + 1, num_vars):
                if rows[i][j]:
                    val ^= x[j]
            x[pc] = val % 2
        
        count = sum(x)
        
        if count < min_count:
            min_count = count
            min_solution = [(r // n, r % n) for r in range(num_vars) if x[r]]
    
    return min_solution if min_solution is not None else []
