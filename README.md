# landed

**Find code that shipped but never runs.**

```bash
landed check --graph .
```

`landed` reads a Rust crate, builds its call graph, and reports functions that
the tests can reach but the running program cannot. Not "unused imports" — whole
features that were written, tested, reviewed, merged, and never connected to
anything.

## The failure it looks for

A feature is built. Tests are written for it. The tests pass. It ships.

And it never runs. Not "has a bug" — nothing ever calls it.

Here is a real one, from the codebase that prompted this tool:

```rust
// The feature: promote a skill once its confidence is high enough.
if trace.confidence >= 0.85 { promote(trace); }

// Production, in another file, written months earlier:
SkillTrace { confidence: 0.0, .. }   // every construction site
```

The condition can never hold. And the test suite contained a helper written to
manufacture the state production cannot produce:

```rust
fn make_promotable_trace(..., confidence: f64, ...) -> SkillTrace
```

Every promotion test called it with `0.9`, `0.95`, `0.88`. All green, for five
months, for a feature that had never once executed.

Code review does not catch this either: a reviewer sees the change in front of
them, not the fact that nothing in a 90,000-line system will ever call it.

## A measurement

Running `landed --graph` over 26 Rust applications — 9 openly built by AI
agents, 17 written by humans:

| | n | Median unreachable | Range |
|---|---:|---:|---|
| AI-built applications | 9 | **2.64%** | 1.59 – 20.97% |
| Human-written applications | 17 | **0.23%** | 0.00 – 2.19% |

Mann-Whitney U = 150 of 153, p = 0.00007.

Matched by size, since larger codebases accumulate more dead code
(r = 0.55 among the human projects):

| Functions | AI median | Human median | Ratio |
|---|---:|---:|---:|
| 600 – 1,300 | 2.25% | 0.25% | 9× |
| 1,300 – 2,300 | 12.38% | 0.68% | 18× |
| 3,000 – 8,000 | 3.25% | 1.35% | 2× |

The shape of the difference matters more than the size of it. Human dead code
is isolated leftovers — one function here, one there. AI dead code arrives as
**connected subsystems**: an entry point plus the helpers it calls, an entire
feature built and never wired in. A per-function check sees only the outermost
function of such a region and reports 1 where the graph reports 40. That is why
this needs reachability rather than a linter.

**Caveats, since they matter more than the number.** n = 26 is small. No human
project in the sample matches the largest AI one (11,530 functions), so the top
of the AI range is uncontrolled. "AI-built" comes from projects' own README
claims. Findings were verified by hand in three codebases, not all.

## Install

```bash
cargo install --git https://github.com/Vanaras-AI/landed
```

## Use

```bash
landed check                     # per-function: does any non-test caller exist?
landed check --graph             # whole-graph reachability (finds dead subsystems)
landed check --dot | dot -Tsvg   # call graph, unreachable nodes in red
landed check --explain my_fn     # every definition and call site for one name
landed check --json              # machine-readable
landed check --graph --fail-over 0   # exit 1 on any finding, for CI
```

`--graph` is the interesting mode. `check` on its own is the conservative one.

## What it deliberately stays quiet about

False positives kill a tool that accuses, so `landed` says nothing unless it is
confident:

- **Trait impl methods** — reachable by dynamic dispatch; no call site proves nothing
- **`#[no_mangle]` / `extern`** — callable from assembly or another language
- **A library's public API** — its callers are in other people's crates
- **`#[allow(dead_code)]`** — the author already decided
- **Names defined more than once** — edges are matched by name, so a collision means silence

The design rule throughout: an approximation must be able to **suppress** a
finding, never to **create** one. Over-count calls and you miss a bug. Over-count
test-ness and you accuse working code — and after two false accusations nobody
runs your tool again.

## Limits

- **Rust only.**
- **Edges are matched by name**, not resolved by type. A method reached only
  through a generic bound may be reported. Every finding names a file and line
  so you can check it, and `--explain` shows the whole picture for one symbol.
- **Entry points are a model, not a fact.** A workspace containing any binary is
  treated as an application whose library crates are internal; a workspace with
  no binary is a library whose entire public API is an entry point. Get this
  wrong and the tool either accuses everything or nothing — both happened during
  development.
- It finds code nothing *calls*. It does not yet find code that is called but
  can never act — the `confidence: 0.0` case above needs type-aware field
  analysis, which is the next check.

## License

MIT OR Apache-2.0
