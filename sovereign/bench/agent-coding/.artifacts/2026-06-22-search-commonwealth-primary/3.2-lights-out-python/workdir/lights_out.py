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
    # The system is Ax = b where x is the vector of presses (length n*n),
    # and b is the initial state flattened.
    # A[i][j] = 1 if pressing cell j affects cell i.
    
    N = n * n
    
    # Build the augmented matrix for GF(2): [A | b]
    # Each row corresponds to a cell that must end up off.
    # Column j represents whether we press cell j.
    aug = [[0] * (N + 1) for _ in range(N)]
    
    for r in range(n):
        for c in range(n):
            idx = r * n + c
            
            # The effect of each possible press on this cell
            for pr in range(n):
                for pc in range(n):
                    pidx = pr * n + pc
                    
                    # Check if pressing (pr, pc) affects (r, c)
                    dist = abs(r - pr) + abs(c - pc)
                    if dist <= 1:
                        aug[idx][pidx] ^= 1
            
            # Target is to turn off the light at (r,c), so b[r*c+n+c] = grid[r][c]
            aug[idx][-1] = grid[r][c]^0

    # Gaussian elimination over GF(2). We want to find x that minimizes sum(x_i).
    # Since we need minimum cardinality, and there might be free variables,
    # we should enumerate solutions in the null space. However, for n<=20, 
    # N=400 which makes enumeration of all null-space combinations potentially expensive if dim(null) is large.
    
    # Standard approach: reduce to row-echelon form first.
    pivot_cols = []
    current_row = 0
    
    for col in range(N):
        # Find a pivot in this column from current_row onwards
        found = -1
        for row in range(current_row, N):
            if aug[row][col] == 1:
                found = row
                break
        
        if found != -1:
            # Swap rows
            aug[current_row], aug[found] = aug[found], aug[current_row]
            
            # Eliminate other rows
            for row in range(N):
                if row != current_row and aug[row][col]:
                    for k in range(col, N + 1):
                        aug[row][k] ^= aug[current_row][k]
            
            pivot_cols.append((current_row, col))
            current_row += 1
    
    rank = len(pivot_cols)
    
    # Check consistency: any row with all zeros on LHS but non-zero RHS is inconsistent.
    for row in range(rank, N):
        if aug[row][-1] == 1:
            return None
    
    # Identify free variables (columns not in pivot_cols)
    pivot_set = set(pc for _, pc in pivot_cols)
    free_vars = [col for col in range(N) if col not in pivot_set]
    
    num_free = len(free_vars)
    
    # If no free variables, unique solution exists. Just read it off.
    if num_free == 0:
        sol = [0] * N
        for r_idx, c_idx in pivot_cols:
            sol[c_idx] = aug[r_idx][-1]
        
        presses = []
        for i in range(N):
            if sol[i]:
                presses.append((i // n, i % n))
        return presses
    
    # With free variables, we need to find the combination that minimizes weight (number of 1s).
    # There are 2^num_free combinations. For n=20, N=400. The null space dimension can be up to ~N/5 or so? 
    # Actually for Lights Out on NxN grid, the nullity is small for most sizes but can be larger for some.
    # However, brute forcing all 2^k solutions where k = num_free might be too slow if k > 20-25.
    
    # Let's check if num_free is manageable (< 30 maybe?). If it's huge, we need a smarter approach (e.g., meet-in-the-middle).
    # But typically for standard grids, nullity is quite low (often < 10).
    
    if num_free > 24:
        # Fallback to heuristic or more complex method? 
        # For now, let's try to optimize the search using bit manipulation and pruning.
        pass

    best_sol = None
    min_weight = N + 1
    
    # Precompute pivot rows mapping col -> row index in reduced matrix
    pivot_row_for_col = {pc: r_idx for r_idx, pc in pivot_cols}
    
    # Iterate over all combinations of free variables
    for mask in range(1 << num_free):
        sol = [0] * N
        
        # Set free variables according to mask
        for i, fv in enumerate(free_vars):
            if (mask >> i) & 1:
                sol[fv] = 1
        
        # Determine dependent variables based on the equations.
        # In RREF form, each pivot equation is: x_pivot + sum(x_free_in_that_eq) = rhs
        # So x_pivot = rhs ^ sum(x_free_in_that_eq).
        
        valid = True
        weight = bin(mask).count('1')
        
        # If current partial weight already exceeds best, skip? 
        # No, because setting free vars to 0 might lead to fewer total presses than a different mask with more free vars but simpler deps.
        # But we can prune if weight >= min_weight since adding dependencies will only increase or keep same.
        if weight >= min_weight:
            continue
            
        for r_idx in range(rank - 1, -1, -1):
            p_row, p_col = pivot_cols[r_idx]
            
            val = aug[p_row][-1]
            for fv in free_vars:
                if sol[fv]:
                    # Check coefficient of this free var in row p_row
                    if aug[p_row][fv]:
                        val ^= 1
            sol[p_col] = val
            
            weight += val
        
        if weight < min_weight:
            min_weight = weight
            best_sol = list(sol)

    presses = []
    for i in range(N):
        if best_sol[i]:
            presses.append((i // n, i %n))
    
    return presses
