# Rust Design Principles

**Types encode the domain. Ownership encodes the architecture. I/O lives at the edge.**

## 1. Model the domain with types

**Make illegal states unrepresentable.**
- Wrap primitives in newtypes with fallible constructors: `struct Email(String)`, `struct OrderId(Uuid)`.
- Use enums for states, commands, and events.
- Use the typestate pattern when the compiler must enforce a transition.

**Parse, don't validate.** Convert untrusted input into domain types once, at the boundary. After that, a value's existence proves it is valid.

**Prefer closed enums over open traits for domain concepts.** Adding a variant should break every `match` that doesn't handle it, which is the point. Use traits when *external* code needs to plug in new implementations.

## 2. Functional core, imperative shell ("sans-IO")

Domain logic is a pure, synchronous state machine. A thin async shell feeds it messages and executes its output against sockets, queues, and databases.

```rust
enum Msg { Deposit(u64), Withdraw(u64) }
enum Event { Deposited(u64), Withdrawn(u64), Rejected(&'static str) }

impl Account {
    // decide: command -> event (pure, no I/O)
    fn decide(&self, msg: Msg) -> Event {
        match msg {
            Msg::Deposit(n) => Event::Deposited(n),
            Msg::Withdraw(n) if n > self.balance => Event::Rejected("insufficient funds"),
            Msg::Withdraw(n) => Event::Withdrawn(n),
        }
    }
    // apply: event -> new state (the reducer)
    fn apply(&mut self, e: &Event) { /* ... */ }
}
```

This shape gives you Redux/Elm-style reducers and CQRS/event sourcing almost for free. It is also testable without mocks or an async runtime.

Reference implementation: `quinn-proto`.

## 3. Let ownership shape the architecture

- **Aim for a tree of ownership** with data flowing through it.
- **Treat widespread `Arc<Mutex<T>>` or `Rc<RefCell<T>>` as a design smell.** The usual fixes are:
  - **Message passing:** one task owns the state; others send it messages.
  - **Single owner plus indices:** store items in a `Vec` or arena and refer to them by ID, not by shared pointer.

## 4. Concurrency: actors as a pattern

An actor is a task that owns its state, plus an `mpsc` receiver, plus a cheap cloneable handle that sends messages. You don't need a framework. See Alice Ryhl's post "Actors with Tokio."

Tokio has no supervision trees. Handle task failure explicitly via `JoinHandle`:
- **Panic** for bugs.
- **`Result`** for expected failures.

Keep the core synchronous. Async belongs in the shell, not spread through the domain.

## 5. Middleware: `tower`

`Service` and `Layer` are the standard abstraction for request pipelines (axum, tonic, hyper). Timeouts, retries, auth, tracing, and rate limiting are layers. Learn it early; it transfers everywhere in async Rust.

## 6. Errors are part of the domain

- **Libraries:** typed error enums, usually via `thiserror`.
- **Applications:** `anyhow` at the top level for context and propagation.
- **Domain errors:** design them with the same care as domain events.

## 7. Abstraction discipline

**Start concrete.** Introduce a trait or generic when a second implementation actually exists, or at a genuine boundary such as storage or a network client.

**Avoid Java-style dependency injection** (`Box<dyn Repository>` threaded everywhere) and "generic soup" (many type parameters on everything).

**Prefer fewer, deeper modules with narrow interfaces** (Ousterhout) over many tiny layers (Clean Code). Document the high-level map in an `ARCHITECTURE.md` (see rust-analyzer's).

**Keep events and queues at real process or service boundaries.** Inside one process, a function call or a typed channel beats an in-process event bus.

## 8. How the classic canon maps

| Principle | In Rust |
|---|---|
| Single Responsibility / Interface Segregation | Hold as-is. Small, focused traits (`Read`, `Write`, `Iterator`). |
| Liskov | Mostly moot. There is no implementation inheritance. |
| Open/Closed | Choose deliberately: closed enums for domain models, open traits for extension points. |
| Dependency Inversion | Apply only at real boundaries, not by default. |
| SICP | Its warnings about shared mutable state are what the borrow checker enforces: aliasing XOR mutation. |

## Further reading

- *Zero to Production in Rust* (Luca Palmieri): a real service, built idiomatically.
- *Rust for Rustaceans* (Jon Gjengset): the next step after the basics.
- Rust API Guidelines: how to shape public interfaces.
- Code to read: `tower`'s `Service` trait, rust-analyzer's `ARCHITECTURE.md`, `quinn-proto`.