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
    
    # Build toggle matrix T where T[i][j] = 1 means pressing cell j affects cell i
    # Cell indices are flattened: idx(r,c) = r*n + c
    def idx(r, c):
        return r * n + c
    
    # For each press position p=(pr, pc), determine which cells it toggles
    presses_to_cells = []
    for pr in range(n):
        for pc in range(n):
            affected = set()
            affected.add(idx(pr, pc))
            if pr > 0: affected.add(idx(pr-1, pc))
            if pr < n-1: affected.add(idx(pr+1, pc))
            if pc > 0: affected.add(idx(pr, pc-1))
            if pc < n-1: affected.add(idx(pr, pc+1))
            presses_to_cells.append(affected)
    
    # Convert grid to vector b (what needs to be toggled mod 2)
    b = []
    for r in range(n):
        for c in range(n):
            b.append(grid[r][c])
    
    # We need to solve T*x �  (mod 2) where x is the press vector
    # Use Gaussian elimination over GF(2)
    num_presses = n * n
    
    # Build augmented matrix [T | b]
    # Each row corresponds to a cell being affected, columns correspond to presses
    matrix = []
    for i in range(num_presses):
        row = [0] * num_presses + [b[i]]
        for j in range(num_presses):
            if i in presses_to_cells[j]:
                row[j] = 1
        matrix.append(row)
    
    # Gaussian elimination with partial pivoting (over GF(2))
    pivot_row = 0
    pivot_cols = []
    
    for col in range(num_presses):
        # Find pivot
        found = -1
        for r in range(pivot_row, num_presses):
            if matrix[r][col] == 1:
                found = r
                break
        
        if found == -1:
            continue
            
        # Swap rows
        matrix[pivot_row], matrix[found] = matrix[found], matrix[pivot_row]
        
        # Eliminate other rows
        for r in range(num_presses):
            if r != pivot_row and matrix[r][col] == 0:
                pass  # Already zero, no need to eliminate
            elif r != pivot_row and matrix[r][col]:
                # XOR the row with pivot row
                for k in range(len(matrix[r])):
                    matrix[r][k] ^= matrix[pivot_row][k]
        
        pivot_cols.append(col)
        pivot_row += 1
    
    # Check consistency (rank of augmented vs coefficient matrix)
    rank = len(pivot_cols)
    
    # Check if any inconsistent equations exist (row has all zeros in coeff but non-zero in b)
    for r in range(rank, num_presses):
        if sum(matrix[r]) > 0:  # Only last element could be non-zero
            return None
    
    # Back-substitute to find solution with minimum weight
    # Free variables are those not in pivot_cols
    free_vars = [c for c in range(num_presses) if c not in pivot_cols]
    
    best_solution = None
    min_weight = float('inf')
    
    # Try all combinations of free variable assignments
    import itertools
    for combo in itertools.product([0, 1], repeat=len(free_vars)):
        x = [0] * num_presses
        
        # Set free variables
        for i, fv in enumerate(free_vars):
            x[fv] = combo[i]
        
        # Back-substitute: solve for pivot variables from bottom to top
        for r_idx in range(rank - 1, -1, -1):
            pc = pivot_cols[r_idx]
            val = matrix[r_idx][-1]  # augmented value (b)
            for c in range(pc + 1, num_presses):
                if matrix[r_idx][c]:
                    val ^= x[c]
            x[pc] = val % 2
        
        weight = sum(x)
        if weight < min_weight:
            min_weight = weight
            best_solution = list(x)
    
    # Convert back to (row, col) format
    result = []
    for i, pressed in enumerate(best_solution):
        if pressed:
            r = i // n
            c = i % n
            result.append((r, c))
    
    return result
