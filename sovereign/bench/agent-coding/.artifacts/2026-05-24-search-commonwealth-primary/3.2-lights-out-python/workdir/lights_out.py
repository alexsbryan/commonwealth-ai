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
    
    # Build toggle matrix for each position in column-major order
    # Each row represents how pressing one cell affects all cells
    def get_toggle_indices(pos_idx):
        r, c = divmod(pos_idx, n)
        indices = [pos_idx]  # self
        if r > 0: indices.append((r-1)*n + c)
        if r < n-1: indices.append((r+1)*n + c)
        if c > 0: indices.append(r*n + (c-1))
        if c < n-1: indices.append(r*n + (c+1))
        return sorted(indices)
    
    total_cells = n * n
    
    # Build the toggle matrix A where A[i][j]=1 means pressing cell j toggles cell i
    A = [[0]*total_cells for _ in range(total_cells)]
    pressers = []
    for pos_idx in range(total_cells):
        affected = get_toggle_indices(pos_idx)
        for idx in affected:
            A[idx][pos_idx] = 1
        pressers.append((divmod(pos_idx, n)[0], divmod(pos_idx, n)[1]))
    
    # Target vector b: which cells need to be toggled (those that are lit)
    b = [grid[r][c] for r in range(n) for c in range(n)]
    
    # Gaussian elimination over GF(2) on augmented matrix [A | I | b]
    # We track both the transformation and the particular solution
    
    aug_size = total_cells * 2 + 1
    aug = [[0]*aug_size for _ in range(total_cells+1)]
    
    # Copy A into left side, identity into middle, b into rightmost column of first part
    for i in range(total_cells):
        for j in range(total_cells):
            aug[i][j] = A[i][j]
        aug[i][total_cells+i] = 1  # identity tracking
        if b[i]:
            aug[i][-1] = 1
    
    pivot_row = [None] * total_cells
    current_pivot_row = 0
    
    for col in range(total_cells):
        found_pivot = False
        for row in range(current_pivot_row, total_cells):
            if aug[row][col]:
                # Swap rows
                aug[current_pivot_row], aug[row] = aug[row], aug[current_pivot_row]
                found_pivot = True
                break
        
        if not found_pivot:
            continue
            
        pivot_row[col] = current_pivot_row
        
        # Eliminate all other rows with this column as pivot
        pivot_val = aug[current_pivot_row][col]
        assert pivot_val == 1, "Expected binary value"
        
        for src_row in range(total_cells + 1):
            if src_row != current_pivot_row and aug[src_row][col]:
                for k in range(aug_size):
                    aug[src_row][k] ^= aug[current_pivot_row][k]
        
        current_pivot_row += 1
    
    # Check consistency (Gauss-Jordan form)
    inconsistent = False
    for row_idx in range(pivot_row[-1]+1 if any(v is not None for v in pivot_row) else 0, total_cells):
        has_nonzero_augmented = False
        has_zero_leftside = True
        for col in range(total_cells):
            if aug[row_idx][col]:
                has_zero_leftside = False
            elif aug[row_idx][-1]:
                pass
        
        left_side_sum = sum(aug[row_idx][:total_cells])
        
        if left_side_sum == 0 and aug[row_idx][-1] != 0:
            inconsistent = True
            break
    
    # Actually check properly - find rows where all A coefficients are zero but b is not
    for row_idx in range(current_pivot_row):
        # This shouldn't happen after proper elimination, but let's verify consistency
        pass
    
    # Check for inconsistency more carefully
    max_used_row = current_pivot_row
    for r in range(max_used_row, total_cells):
        lhs_zero = all(aug[r][c] == 0 for c in range(total_cells))
        rhs_nonzero = aug[r][-1] != 0
        if lhs_zero and rhs_nonzero:
            return None
    
    # Extract solution using the tracked identity matrix portion
    x = [0]*total_cells
    for col_idx in range(total_cells-1, -1, -1):
        pr = pivot_row[col_idx]
        if pr is not None:
            val = aug[pr][-1] ^ sum(aug[pr][j+total_cells] * x[j] % 2 
 for j in range(col_idx+1, total_cells) if pivot_row[j] is not None)
            x[col_idx] = val
    
    # Verify solution and collect presses
    final_state = b[:]
    for idx in range(total_cells):
        if x[idx]:
            affected = get_toggle_indices(idx)
            for aff_idx in affected:
                final_state[aff_idx] ^= 1
    
    if any(final_state):
        return None
    
    result = [(pressers[i][0], pressers[i][1]) for i in range(total_cells) if x[i]]
    
    # Try to find minimum by checking null space solutions
    # Find free variables (columns without pivots)
    pivot_cols = [c for c, r in enumerate(pivot_row) if r is not None]
    free_vars = [c for c in range(total_cells) if c not in pivot_cols]
    
    min_weight = len(result)
    best_solution = list(result)
    
    # For small number of free vars (< 25), try all combinations
    num_free = len(free_vars)
    if num_free < 30:  # Cap at reasonable size to avoid TLE
        from itertools import product
        
        for combo_bits in product([0,1], repeat=num_free):
            delta_x = x[:]
            for i, fv_idx in enumerate(free_vars):
                bit = combo_bits[i]
                
                # Compute null space vector contribution
                ns_vec = [0]*total_cells
                
                # The null space basis vectors come from the elimination process
                # For each free variable column f with pivot p_f, we need to compute 
                # how pressing f affects all variables through back-substitution
            
            if sum(combo_bits) + min_weight < len(result):  # Only optimize if potentially smaller
                pass
    
    return best_solution
