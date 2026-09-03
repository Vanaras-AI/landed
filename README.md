# landed

**Find code that shipped but never runs.**

```bash
landed check --graph .
```

`landed` reads a Rust crate, builds its call graph, and reports functions the
tests can reach but the running program cannot. Not unused imports — whole
features that were written, tested, reviewed, merged, and never connected to
anything.

## The failure it looks for

A feature is built. Tests are written for it. They pass. It ships.

And it never runs. Not "has a bug" — nothing ever calls it.

A real example:

```rust
// The feature: promote a skill once its confidence is high enough.
if trace.confidence >= 0.85 { promote(trace); }

// Production, in another file, written months earlier:
SkillTrace { confidence: 0.0, .. }   // every construction site
```

The condition can never hold. And the test suite contained a helper built to
manufacture the state production does not produce:

```rust
fn make_promotable_trace(..., confidence: f64, ...) -> SkillTrace
```

Every promotion test called it with `0.9`, `0.95`, `0.88`. All green, for five
months, for a feature that had never executed.

Tests do not catch this, because the fixtures are chosen by whoever wrote the
code. Review does not catch it either: a reviewer sees the change in front of
them, not the fact that a literal written in another file months earlier makes
the new condition unreachable.

## Why a graph

The cheap check is "does any non-test caller exist?" It finds only the
outermost function of a dead region:

```rust
fn main() { live_thing(); }

pub fn dead_entry() { helper(); }   // flagged
fn helper()         { deeper(); }   // not flagged
fn deeper()         { }             // not flagged
```

All three are unreachable. `helper` has a caller — the caller just happens to
be dead too. So `landed` builds the call graph and computes reachability from
real entry points, which reports the whole region instead of its tip.

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
landed check --json              # versioned JSON envelope
landed check --format github     # annotations that land on the PR diff
landed check --graph --fail-over 0   # exit 1 on any finding, for CI
```

`--graph` is the interesting mode. `check` alone is the conservative one.

## Adopting it on a codebase that already has findings

A crate that has never been analysed will produce findings on the first run —
one produced 216. Presented as a wall, that is uninstallable: the honest
response is to remove the tool, not to fix 216 functions.

Record what is already there, and gate on what is added:

```bash
landed baseline            # writes .landed-baseline.json — commit it
landed check --baseline    # exits 1 only on findings not in the baseline
```

The baseline names findings by function and file, never by line, so an
unrelated edit that shifts code down a file does not resurface them. Findings
that disappear are reported as cleared, so the backlog can be paid down
visibly. A baseline taken with `--graph` is refused against a per-function run
and vice versa — otherwise the difference between two analyses would be
reported as a change in the code.

In CI, with annotations on the diff rather than in log output nobody opens:

```yaml
- run: cargo install --git https://github.com/Vanaras-AI/landed
- run: landed check --graph --baseline
- run: landed check --graph --format github    # annotate the source lines
```

Confident findings annotate as `warning`; uncertain ones as `notice`, because
a finding the analyzer is unsure about should not decorate a diff with the
same weight as one it can prove. Regions annotate once, at the frontier —
forty annotations for one dead subsystem buries the review.

## Telling it what production means

No heuristic finds every entry point. A handler spawned as a task or held in a
registry breaks the chain from `main`, and everything downstream is then
reported dead — on one codebase that wrongly included a live PII-redaction
subsystem. Say what the analyzer cannot infer:

```toml
# landed.toml, beside Cargo.toml
roots  = ["handle_webhook", "daemon_process_*"]   # entry points it cannot see
ignore = ["generated_*", "legacy_shim"]           # never report these
```

A declared root outranks every heuristic, and everything reachable from it
becomes live. Patterns take `*`. A malformed or misspelled key is an error
rather than a silent fallback to no config — otherwise the declarations
vanish and the tool starts condemning live code again with no indication why.

## JSON is an API

`--json` emits a versioned envelope, not a dump of the text output:

```json
{
  "schema": 1,
  "tool": "landed",
  "mode": "graph",
  "summary": {
    "production_functions": 1997,
    "unanalysable_names": 777,
    "unreachable": 216,
    "confident": 112,
    "uncertain": 49,
    "regions": 161
  },
  "regions": [ ... ]
}
```

`unanalysable_names` is in the summary deliberately: a total reported without
a coverage figure overstates itself.

## What it deliberately stays quiet about

False positives kill a tool that accuses, so `landed` says nothing unless it is
confident:

- **Trait impl methods** — reachable by dynamic dispatch; no call site proves nothing
- **`#[no_mangle]` / `extern`** — callable from assembly or another language
- **A library's public API** — its callers are in other crates
- **`#[allow(dead_code)]`** — the author already decided
- **Names defined more than once** — edges are matched by name, so a collision means silence

The rule throughout: an approximation must be able to **suppress** a finding,
never to **create** one. Over-count calls and you miss a bug. Over-count
test-ness and you accuse working code — and after two false accusations nobody
runs your tool again.

## Limits

- **Rust only.**
- **Edges are matched by name**, not resolved by type. A method reached only
  through a generic bound may be reported. Every finding names a file and line,
  and `--explain` shows the whole picture for one symbol.
- **Entry points are a model, not a fact.** A workspace containing any binary is
  treated as an application whose library crates are internal; a workspace with
  no binary is a library whose entire public API is an entry point. Get this
  wrong and the tool either accuses everything or nothing — both happened during
  development.
- It finds code nothing *calls*. It does not yet find code that is called but
  can never act — the `confidence: 0.0` case above needs type-aware field
  analysis, which is the next check.

If it reports something genuinely live, that is a bug and the report is welcome.

## License

MIT OR Apache-2.0
