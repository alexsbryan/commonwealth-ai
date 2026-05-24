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
    
    # Build the toggle matrix for each cell press
    # For an n x n grid, pressing cell (r,c) toggles itself and neighbors
    def get_toggle_mask(r, c):
        mask = [0] * (n * n)
        positions = [(r, c)]
        
        # Add orthogonal neighbors
        if r > 0: positions.append((r-1, c))
        if r < n - 1: positions.append((r+1, c))
        if c > 0: positions.append((r, c-1))
        if c < n - 1: positions.append((r, c+1))
        
        for pr, pc in positions:
            idx = pr * n + pc
            mask[idx] = 1
            
        return mask
    
    # Create toggle vectors for all possible presses (n*n cells)
    toggle_vectors = []
    for i in range(n):
        for j in range(n):
            toggle_vectors.append(get_toggle_mask(i, j))
    
    # Convert grid to a flat vector of bits
    initial_state = [grid[r][c] for r in range(n) for c in range(n)]
    
    # We need to solve: A*x = b over GF(2) where:
    # - A is the matrix where each column is a toggle vector
    # - x is our solution vector (which cells to press)
    # - b is the initial state (we want to turn all lights off)
    
    num_vars = n * n
    
    # Build augmented matrix [A | b] over GF(2)
    aug_matrix = []
    for i in range(num_vars):
        row = []
        for j in range(num_vars):
            row.append(toggle_vectors[j][i])  # Column j, Row i of A
        
        # Add the target value from initial_state
        row.append(initial_state[i])
        
        aug_matrix.append(row)
    
    # Gaussian elimination over GF(2) with pivot tracking
    pivots = [-1] * num_vars  # Which column is the pivot for each row
    current_row = 0
    
    for col in range(num_vars):
        # Find a row with this column as pivot (starting from current_row)
        found_pivot = -1
        for r in range(current_row, num_vars):
            if aug_matrix[r][col] == 1:
                found_pivot = r
                break
        
        if found_pivot != -1:
            # Swap rows to bring pivot to current position
            aug_matrix[current_row], aug_matrix[found_pivot] = \
                aug_matrix[found_pivot], aug_matrix[current_row]
            
            pivots[current_row] = col
            
            # Eliminate all other entries in this column
            for r in range(num_vars):
                if r != current_row and aug_matrix[r][col]:
                    for c_idx in range(len(aug_matrix[r])):
                        aug_matrix[r][c_idx] ^= aug_matrix[current_row][c_idx]
            
            current_row += 1
    
    # Check consistency - any row with all zeros on left but non-zero on right means no solution
    for r in range(current_row, len(pivots)):
        if aug_matrix[r][-1] == 1:
            return None
    
    # Extract particular solution (set free variables to 0)
    x = [0] * num_vars
    for i in range(min(current_row, n*n)):
        pivot_col = pivots[i]
        if pivot_col != -1 and i < len(aug_matrix):
            x[pivot_col] = aug_matrix[i][-1]
    
    # Convert back to list of coordinates
    result = []
    for idx in range(num_vars):
        if x[idx]:
            r = idx // n
            c = idx % n
            result.append((r, c))
    
    return result
