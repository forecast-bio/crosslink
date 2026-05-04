---
name: rust-quality
description: Use any time you write, review, refactor, or design Rust code (anything in a `Cargo.toml`-bearing crate, `.rs` files, library APIs, async/tokio, error types, traits, unsafe blocks, build scripts). Enforces idiomatic API design (Rust API Guidelines), strict error handling (`Result` + `thiserror`/`anyhow`), clippy lint discipline, safe-`unsafe` with SAFETY comments + miri, structured-concurrency rules for async, and the rustdoc / testing / module-structure baselines that distinguish "compiles" from "shippable." Trigger when the user mentions Rust, asks "is this idiomatic", requests a Rust code review, opens a `.rs` file for editing, or works in a Cargo project. Pair with `rust-gpu-discipline` whenever the work also touches CUDA / wgpu / Metal / cubecl / cudarc / kernels.
---

# Rust quality — universal discipline for any Rust crate

This skill makes the model write idiomatic, well-tested, lint-clean Rust by default — not just code that compiles. It applies to libraries, binaries, sync code, async/tokio, `no_std`, embedded, wasm, and GPU host code. (For GPU kernel work, also load `rust-gpu-discipline`.)

## Scope check before you start

Before writing or reviewing Rust, identify:

- **Crate kind**: library (lib.rs) or binary (main.rs) — error-handling discipline differs.
- **Sync or async**: presence of `tokio`, `async-std`, or `async fn` in the surface area.
- **`unsafe` posture**: does the crate use `unsafe`? If so, miri is mandatory.
- **MSRV**: read `rust-version` in `Cargo.toml`. Don't use newer features.
- **Existing lint config**: read `Cargo.toml` `[lints]` table and any `#![…]` crate attributes in `lib.rs` / `main.rs` before adding new ones.

If any of the above is unclear, read the manifest and the crate root file. **Do not guess.**

---

## 1. API design

Rules:

- Follow RFC 430 casing: `UpperCamelCase` types, `snake_case` functions, `SCREAMING_SNAKE_CASE` consts.
- Use conversion prefixes consistently: `as_` (cheap borrow), `to_` (expensive, non-consuming), `into_` (consuming).
- Name iterator methods `iter` / `iter_mut` / `into_iter`; iterator types match the producer name (`Foo` → `FooIter`).
- Implement the common trait set when semantics allow: `Debug`, `Clone`, `Default`, `PartialEq`/`Eq`, `Hash`, `Display`, plus `Send` + `Sync`.
- Implement `Debug` on every public type and never produce empty `Debug` output.
- Keep struct fields private; constructors and getters are the public surface so internals can evolve.
- Seal extension traits (`pub trait T: sealed::Sealed`) when downstream impls would break invariants.
- Hide implementation in newtypes (`pub struct Handle(Inner);`) — do not leak third-party types in public signatures.
- Don't add inherent methods to smart-pointer wrappers (`Arc<T>`, `Box<T>`); methods belong on the inner type.

Source: <https://rust-lang.github.io/api-guidelines/checklist.html>

## 2. Idiomatic patterns

Rules:

- Wrap primitive-typed semantic values in newtypes (`struct UserId(u64)`) so illegal calls fail at compile time.
- Implement `Drop` for any type that owns a resource (file, lock, GPU buffer, FFI handle); never expose a manual `close()` as the only release path.
- Use the builder pattern only when a constructor would need >4 args or many optional fields; otherwise `Default` + struct update syntax.
- Implement `From<A> for B` for infallible conversions; `TryFrom<A> for B` for fallible. Never implement `Into` directly — it is blanket-derived.
- Accept the borrowed type, not a reference to the owned type: `&str` over `&String`, `&[T]` over `&Vec<T>`, `&Path` over `&PathBuf`.
- Prefer `impl Trait` for arguments when monomorphization is fine; use a generic `<T: Trait>` when the type appears in multiple positions; use `dyn Trait` only when heterogeneous storage or dynamic dispatch is required.
- Encode state machines with the typestate pattern (`Connection<Open>` vs `Connection<Closed>`) so invalid transitions fail to compile.

Source: <https://www.lurklurk.org/effective-rust/>

## 3. Error handling

Rules:

- Return `Result<T, E>` for any fallible operation. Reserve `panic!` for genuine invariant violations.
- Use `?` for propagation; rely on `From` impls to convert error types instead of `match` / `map_err` ladders.
- In **library** crates: define a concrete error enum with `thiserror` — `#[derive(Error)]`, `#[error("…")]`, `#[from]` for free conversions, `#[source]` to chain causes. `thiserror` does not appear in your public API.
- In **binary** crates: return `anyhow::Result<T>` and attach `.context("…")` / `.with_context(|| …)` at every logical boundary.
- Never mix the two: don't put `anyhow::Error` in a library's public API; don't hand-roll error enums in a small binary.
- Forbid `unwrap()` / `expect()` in non-test code via `clippy::unwrap_used` / `expect_used`. Use `expect("invariant: …")` only where failure is provably impossible and the `expect` message documents why.
- Every error type must implement `std::error::Error` (and therefore `Debug` + `Display`); preserve the cause chain via `source()`.

Source: <https://blog.burntsushi.net/rust-error-handling/>

## 4. Lint discipline

Rules:

- Crate-level baseline: `#![warn(clippy::all, clippy::pedantic)]`, `#![deny(unsafe_code)]` (override per-module with `#[allow(unsafe_code)]` plus a SAFETY justification), `#![deny(rust_2018_idioms, missing_debug_implementations)]`. Libraries also `#![deny(missing_docs)]`.
- Treat the `correctness` group as deny — it catches real bugs.
- Opt into these pedantic lints: `needless_pass_by_value`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`, `semicolon_if_nothing_returned`, `redundant_closure_for_method_calls`, `items_after_statements`, `match_wildcard_for_single_variants`.
- Opt into these restriction lints (allow-by-default but worth enforcing): `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `print_stdout` (libs), `clone_on_ref_ptr`.
- Reject blanket `#[allow(...)]` at the crate root. Allow narrowly at the item with a comment justifying it.
- Run `cargo clippy --all-targets --all-features -- -D warnings` in CI; a warning is a failure.

Source: <https://rust-lang.github.io/rust-clippy/master/>

## 5. Unsafe code

Rules:

- `unsafe` only unlocks five powers: dereference raw pointers, call `unsafe` fns, implement `unsafe` traits, mutate `static mut`, read union fields. If you don't need one of those, you don't need `unsafe`.
- Uphold every invariant: no dangling/misaligned pointers, no reads of uninitialized memory, valid bit patterns (`bool` is 0/1, `char` is a valid scalar), no aliased `&mut`, no data races, ABI must match.
- Every `unsafe { … }` block requires a `// SAFETY: …` comment that names the invariant and proves it locally.
- Wrap unsafe in a safe abstraction: the `unsafe` keyword should appear inside a module, not in user-facing signatures, unless the caller genuinely must uphold something.
- Run `cargo +nightly miri test` against any crate containing `unsafe`. Treat any miri diagnostic as a release blocker.
- Document `# Safety` sections on every `pub unsafe fn` listing the caller's obligations.

Source: <https://doc.rust-lang.org/nomicon/what-unsafe-does.html>

## 6. Performance defaults

Rules:

- Prefer iterator chains over index loops — they elide bounds checks and fuse.
- Reject every `.clone()` that is not load-bearing. Clone at the call site, not inside the callee.
- Take `&str` / `&[T]` / `&Path` parameters; return `String` / `Vec<T>` only when ownership transfer is real.
- Use `Vec::with_capacity(n)` / `String::with_capacity(n)` whenever the final size is bounded or known.
- Use `Cow<'a, str>` (or `Cow<'a, [T]>`) when a function sometimes mutates and sometimes returns its input unchanged.
- Use `Box<dyn Trait>` only when you need heterogeneous storage or dyn-stable ABI; default to generics for hot paths.
- Don't pre-optimize — but don't ship `O(n²)` `Vec::contains` membership checks or `format!` in tight loops.

Source: <https://rust-unofficial.github.io/patterns/idioms/coercion-arguments.html>

## 7. Testing

Rules:

- Unit tests live in `#[cfg(test)] mod tests { use super::*; }` at the bottom of each module; integration tests live in `tests/<name>.rs` and exercise only the public API.
- Every public function with non-trivial logic needs at least one test. Bug fixes always land with a regression test.
- Write doctests for the canonical use of every public item — they double as compile-checked documentation.
- Use `proptest` (or `quickcheck`) for any function with an algebraic property (round-trip, idempotence, monotonicity); pure parsers and serializers are mandatory targets.
- Use `criterion` for benchmarks (not the built-in `#[bench]`); commit baseline numbers and gate regressions in CI.
- Use `insta` snapshot tests for human-readable output (rendered errors, generated code, CLI help); review snapshot diffs before accepting.
- `#[should_panic(expected = "…")]` must always pin the expected message — bare `#[should_panic]` masks unrelated panics.

Source: <https://doc.rust-lang.org/book/ch11-00-testing.html>

## 8. Documentation

Rules:

- `#![deny(missing_docs)]` on every library crate; run `cargo doc --no-deps -- -D warnings` in CI.
- Every public item gets a rustdoc comment with: one-line summary, blank line, detailed prose, `# Examples`, then `# Errors` (for `Result`-returning fns), `# Panics` (for fns that can panic), `# Safety` (for `unsafe fn`).
- The summary line is one sentence, reads as a noun phrase or imperative — it appears in search results.
- Every example must compile. Mark non-compiling fragments `ignore` and explain why.
- Use `//!` for crate- and module-level docs in `lib.rs` / `mod.rs`; use `///` for items.
- Cross-link with intra-doc links (`` [`Foo`] ``, `` [`bar()`](crate::bar) ``); never paste raw URLs to your own items.

Source: <https://doc.rust-lang.org/rustdoc/how-to-write-documentation.html>

## 9. Module / crate structure

Rules:

- `lib.rs` declares modules and re-exports the public surface; consumers should never need to know your internal module tree.
- Provide a `prelude` module (`pub mod prelude { pub use crate::{Foo, Bar, Trait}; }`) for the items 90% of users want with one glob import.
- Feature flags must be **purely additive** — enabling a feature can add items, never remove or change them, and never change behavior of existing items. No mutually exclusive features.
- Declare MSRV in `Cargo.toml` (`rust-version = "1.XX"`) and verify it in CI with that toolchain. Bumping MSRV is a minor-version event.
- Split into a workspace when (a) crates have meaningfully different dependency sets, (b) one crate is `no_std` and another isn't, or (c) compile time of the monolith exceeds developer patience. Don't pre-split.
- Keep `proc-macro` crates separate from the crate that uses them; same for `*-sys` FFI bindings vs. their safe wrappers.

Source: <https://doc.rust-lang.org/cargo/reference/features.html>

## 10. Async / tokio gotchas

Apply only when the code uses `async fn`, `tokio`, `async-std`, or `futures`.

Rules:

- Never call blocking code (`std::fs`, `std::thread::sleep`, CPU-bound loops, sync mutexes under contention) directly inside an `async fn` — wrap with `tokio::task::spawn_blocking` and `.await` the join handle.
- Futures spawned via `tokio::spawn` must be `Send + 'static`; if you can't satisfy that, use `tokio::task::spawn_local` on a `LocalSet`.
- Never hold a `std::sync::MutexGuard` across an `.await` — drop the guard in an inner scope first. Use `tokio::sync::Mutex` only when the lock genuinely must span an await point, and accept the overhead.
- Every branch in `tokio::select!` must be **cancel-safe**: dropping the future mid-poll must be a no-op, otherwise progress is silently lost on every loop iteration. When a future is not cancel-safe, store it via `tokio::pin!` outside the loop and pass `&mut fut` into `select!`.
- For lifetime-bound futures (`async fn(&self)`), spawning requires `Arc<Self>` or scoped tasks (`tokio::task::JoinSet` with owned data). You cannot spawn a borrow.
- `spawn_blocking` tasks cannot be aborted — design them to check a `CancellationToken` if cooperative shutdown is required.
- Prefer structured concurrency primitives (`JoinSet`, `tokio_util::sync::CancellationToken`) over raw `JoinHandle` juggling for graceful shutdown.

Source: <https://tokio.rs/tokio/tutorial/select>

---

## Forbidden-pattern checklist (mechanical scan)

Before reporting Rust work as complete, grep the diff for these and either fix or justify each hit:

| Pattern | Why it's flagged |
|---|---|
| `.unwrap()` / `.expect(` outside `#[cfg(test)]` / `tests/` / `examples/` / `main.rs` happy-path startup | Panic on error path; use `?` and a real error type. |
| `panic!(` in library code | Move to `Result::Err(...)` unless it's a genuinely-impossible invariant. |
| `todo!()` / `unimplemented!()` | Don't ship stubs. |
| `dbg!(` | Debug leftover. |
| `println!` in a library crate (non-CLI) | Use `tracing` / `log` / return data. |
| `.clone()` on a parameter immediately after entry | Caller should clone. |
| `&String` / `&Vec<T>` / `&PathBuf` parameters | Use `&str` / `&[T]` / `&Path`. |
| `unsafe {` without a preceding `// SAFETY:` line | Every unsafe block needs a written justification. |
| `tokio::spawn(async move { … })` capturing `&self` | Lifetimes won't hold; use `Arc<Self>` or `JoinSet`. |
| `std::sync::Mutex` lock guard in scope across `.await` | Deadlock waiting to happen; drop the guard in an inner block. |
| `#[allow(...)]` at crate root | Move to the item with a justifying comment. |
| `pub` field on a struct in a library | Make it private; expose getters/constructors. |
| `Box<dyn Trait>` in a hot loop | Generics + monomorphization unless heterogeneity is actually required. |

### Notes on specific patterns

**`println!` / `eprintln!` removal vs. replacement.** The default action is to replace with `tracing::warn!` / `log::warn!`. But before adding `tracing` or `log` as a new dependency just for one warning, check the workspace `Cargo.toml`:

- If the workspace already declares `tracing` (or `log`) in `[workspace.dependencies]`, use it. Add the crate-local dep with `tracing.workspace = true`.
- If neither is in the workspace, **prefer removal over adding the dep** when (a) the message duplicates information the runtime or test harness already surfaces, (b) the print is in test-only code (`#[cfg(test)]`), or (c) the print is decorative ("starting", "skipping", "done"). Adding a logger crate solely for one print is a workspace-coordination event and disproportionate to the value.
- If the message carries genuinely actionable information that the user needs but isn't available elsewhere, surface it via the function's `Result` return (turn the print into part of an `Err` variant or a structured event the caller can subscribe to), not via stdout/stderr.

The point is to avoid a leaf-crate subagent unilaterally adding `tracing` to satisfy a lint when the right move is removal.

## Honest reporting

When you finish a Rust change, your report must include:

1. `cargo build` result.
2. `cargo clippy --all-targets --all-features -- -D warnings` result. If you allowed any lint, name it and justify.
3. `cargo test` result. Include doctest count; if you added a public item, you must have added a doctest.
4. `cargo fmt --check` result.
5. If `unsafe` was added or modified: `cargo +nightly miri test` result.
6. If a public API was changed: confirm the SemVer impact (patch / minor / major) and whether MSRV moved.

If any step was skipped, **say so explicitly** rather than implying success. "I didn't run miri because nightly isn't installed" is acceptable; silently omitting it is not.

## Pre-flight planning prompt (use before non-trivial work)

Before writing more than ~30 lines of Rust, answer in your own head:

1. What is the **error type** for this code path, and which crate (library → `thiserror` enum, binary → `anyhow::Result`)?
2. What's the **ownership story** — who owns each value, who borrows, where does it cross an `.await` (if async)?
3. What **invariants** does this code rely on? Are any of them strong enough to need a newtype or typestate?
4. What's the **public surface**? Is anything `pub` that doesn't need to be?
5. What's the **test strategy** — unit, integration, doctest, proptest? At minimum one regression test per behavior.
6. Are there any **clippy lints** I expect to fight? If so, decide upfront whether to refactor or `#[allow(...)]` with justification.

Skipping this planning is the most common cause of "it compiles but it's not idiomatic Rust."
