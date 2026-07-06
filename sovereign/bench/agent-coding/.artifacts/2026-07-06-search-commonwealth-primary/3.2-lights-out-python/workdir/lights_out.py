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
    
    # We use Gaussian elimination over GF(2) to find a solution.
    # The system is Ax = b where x is the vector of presses and b is the initial state.
    # Each press toggles itself and neighbors.
    
    size = n * n
    
    # Build the matrix A (size x size) and vector b (size,)
    # A[i][j] = 1 if pressing cell j affects cell i
    
    # Map (r, c) -> index r*n + c
    def idx(r, c):
        return r * n + c
    
    # Initialize A as list of lists for Gaussian elimination
    # We'll work with augmented matrix [A | b]
    aug = [[0] * (size + 1) for _ in range(size)]
    
    for r in range(n):
        for c in range(n):
            col = idx(r, c)
            # Pressing (r,c) toggles:
            neighbors = [(r, c), (r-1, c), (r+1, c), (r, c-1), (r, c+1)]
            for nr, nc in neighbors:
                if 0 <= nr < n and 0 <= nc < n:
                    row = idx(nr, nc)
                    aug[row][col] ^= 1
    
    # Set b vector from grid
    for r in range(n):
        for c in range(n):
            row = idx(r, c)
            aug[row][size] = grid[r][c]
    
    # Gaussian elimination over GF(2) with partial pivoting to find minimum weight solution
    pivot_col = [0] * size  # which column is the pivot for each row
    pivot_row_for_col = [-1] * size  # which row has the pivot for each column (if any)
    
    current_row = 0
    for col in range(size):
        # Find a row with a 1 in this column at or below current_row
        found = -1
        for r in range(current_row, size):
            if aug[r][col]:
                found = r
                break
        
        if found == -1:
            continue  # Free variable
        
        # Swap rows
        aug[current_row], aug[found] = aug[found], aug[current_row]
        
        # Eliminate all other rows
        for r in range(size):
            if r != current_row and aug[r][col]:
                for c2 in range(size + 1):
                    aug[r][c2] ^= aug[current_row][c2]
        
        pivot_col[current_row] = col
        pivot_row_for_col[col] = current_row
        current_row += 1
    
    rank = current_row
    
    # Check consistency: any row with all zeros on left but 1 on right is inconsistent
    for r in range(rank, size):
        if aug[r][size]:
            return None
    
    # Now we have a reduced system. We need to find the minimum weight solution.
    # Variables not used as pivots are free variables. Set them and enumerate combinations.
    
    pivot_cols_set = set()
    for r in range(rank):
        pivot_cols_set.add(pivot_col[r])
    
    free_vars = [c for c in range(size) if c not in pivot_cols_set]
    num_free = len(free_vars)
    
    best_solution = None
    min_weight = float('inf')
    
    # Enumerate all 2^num_free possibilities for free variables
    for mask in range(1 << num_free):
        x = [0] * size
        
        # Set free variables according to mask
        for i, fv in enumerate(free_vars):
            x[fv] = (mask >> i) & 1
        
        # Back-substitute to find pivot variable values
        valid = True
        for r in range(rank - 1, -1, -1):
            pc = pivot_col[r]
            val = aug[r][size]
            for c2 in range(pc + 1, size):
                if aug[r][c2]:
                    val ^= x[c2]
            x[pc] = val
        
        weight = sum(x)
        if weight < min_weight:
            min_weight = weight
            best_solution = list(x)
    
    # Convert solution vector to list of (row, col) presses
    result = []
    for i in range(size):
        if best_solution[i]:
            r = i // n
            c = i % n
            result.append((r, c))
    
    return result

