# landed

**Find code that shipped but never runs.**

```
landed check .
```

## The problem

An AI agent writes a feature. It writes the tests for that feature. It reports
success. CI is green, the PR is reviewed, the PR is merged.

And the feature never runs. Not "has a bug" — it is never reached, by anything,
ever.

This is invisible to every check you already have, because the agent wrote the
code *and* the tests *and* chose the fixtures. All three came from the same
process with the same blind spot, so of course they agree with each other.

A real example, from the codebase that motivated this tool:

```rust
// The feature: promote a skill when its confidence is high enough.
if trace.confidence >= 0.85 { promote(trace); }

// Production, written months earlier, in a different file:
SkillTrace { confidence: 0.0, .. }   // every construction site
```

The condition can never be true. The feature has never fired.

And the test suite? It contains a helper the agent wrote to manufacture the
state production cannot produce:

```rust
fn make_promotable_trace(..., confidence: f64, ...) -> SkillTrace
```

Every promotion test calls it with `0.9`, `0.95`, `0.88`. All green. For five
months.

## What landed does

It reads your crate and reports functions that exist in production code and are
called **only** by tests.

```
NEVER RUN — defined in production, called only by tests
──────────────────────────────────────────────────────────

  handle_a2a_request
    defined  src/a2a.rs:270
    callers  3 test call(s), 0 production calls

  detect_stuck
    defined  src/stuck_detector.rs:259
    callers  7 test call(s), 0 production calls
```

That's it. No LLM, no heuristics, no opinions — a claim you can check by hand in
thirty seconds, which is exactly what makes it safe to act on.

## Install

```bash
cargo install --git https://github.com/Vanaras-AI/landed
```

Or build from source:

```bash
git clone https://github.com/Vanaras-AI/landed && cd landed
cargo build --release
./target/release/landed check /path/to/crate
```

## Usage

```bash
landed check                      # scan current directory
landed check ./src                # scan a path
landed check --json               # machine-readable output
landed check --fail-over 10       # exit 1 if more than 10 findings (for CI)
```

## What it deliberately does not flag

False positives destroy a tool like this, so `landed` stays quiet unless it is
confident:

- **Trait impl methods** (`impl Trait for Type`) — reachable by dynamic
  dispatch, so no direct call site proves nothing.
- **`#[allow(dead_code)]`** — the author already made this call.
- **Ambiguous names** — if two production functions share a name, name-based
  matching can't tell the call sites apart, so it says nothing.
- **Conventional entry points** — `main`, `new`, `Default::default`, `fmt`,
  serde hooks, iterator methods.

## Known limits

- **Rust only** for now.
- **Name-based call matching.** A method called only through a generic bound or
  a function pointer may be reported. Check before acting; every finding names
  a file and line for exactly that reason.
- **Public library API.** A `pub fn` intended for downstream consumers has no
  in-crate caller by design. On a library crate, read findings as "unused
  *here*", not "unused everywhere".
- It finds code that is never *called*. It does not yet find code that is
  called but can never do anything — the `confidence: 0.0` case above needs
  type-aware dataflow, which is the next check.

## Why this exists

It was written after auditing an 86K-line Rust codebase that an autonomous
agent had maintained for six months. That codebase had 1,169 passing tests,
53 merged agent-authored PRs, and 123 functions that had never run — including
its entire agent-to-agent protocol, its stuck-detector, its privacy budget
enforcement, and its Telegram output escaping.

Nobody had looked, because nobody had the thing that looks.

## License

MIT OR Apache-2.0
