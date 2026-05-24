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
    
    # Convert to flat state representation for efficiency
    initial_state = []
    for r in range(n):
        for c in range(n):
            initial_state.append(grid[r][c])
    
    # For each possible first row press pattern (2^n possibilities),
    # determine subsequent presses deterministically.
    best_solution = None
    
    def apply_press(state, r, c):
        """Toggle cell and neighbors."""
        new_state = list(state)
        dirs = [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)]
        for dr, dc in dirs:
            nr, nc = r + dr, c + dc
            if 0 <= nr < n and 0 <= nc < n:
                idx = nr * n + nc
                new_state[idx] ^= 1
        return tuple(new_state)
    
    # Enumerate all possible first-row press patterns
    for mask in range(2 ** n):
        presses = []
        
        # Apply first row presses based on mask
        current_state = list(initial_state)
        for c in range(n):
            if (mask >> c) & 1:
                presses.append((0, c))
                current_state = apply_press(current_state, 0, c)
        
        # For rows 1 to n-1, determine presses needed to fix previous row lights
        for r in range(1, n):
            for c in range(n):
                idx_prev_row = (r - 1) * n + c
                if current_state[idx_prev_row] == 1:
                    # Need to press below this cell to turn it off
                    presses.append((r, c))
                    current_state = apply_press(tuple(current_state), r, c)
        
        # Check if all lights are now off
        all_off = True
        for i in range(len(current_state)):
            if current_state[i] != 0:
                all_off = False
                break
        
        if all_off:
            if best_solution is None or len(presses) < len(best_solution):
                best_solution = list(presses)
    
    return best_solution
