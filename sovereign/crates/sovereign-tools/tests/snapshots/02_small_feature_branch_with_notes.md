# Project context: fixture @ feature/auth-rate-limiting

## Working set

- `src/auth/proxy.rs`
- `src/auth/loopback_guard.rs`
- `src/auth/middleware.rs`

## Stated about this area

- **[decision]** _?_ Auth flows route through loopback_guard. RFC-0017.
- **[invariant]** _?_ No plaintext credentials in logs at any layer.

