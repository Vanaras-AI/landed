# Precise mode

```bash
landed check --graph --precise
```

Resolves calls with the compiler instead of by name. Opt-in, because it needs
things the default deliberately does not.

## Requirements

| | |
|---|---|
| Nightly toolchain | `rustup toolchain install nightly` — MIR is emitted through `-Zunpretty`, an unstable flag |
| A crate that compiles | MIR exists only for code the compiler accepted |
| A cargo project | there must be something to build |

Each is checked before any work and reported with what to do about it.
**Precise mode never falls back to the nominal analysis.** A mode whose entire
purpose is precision that quietly answers with less is worse than one that
refuses.

## What it fixes

The default frontend keys the call graph on function names, so `A::process`
and `B::process` are one node. The analysis then declines to judge either,
because a wrong accusation costs more than a missed finding. That silence
covers **38–43% of a real codebase**.

```rust
impl A { pub fn run(&self) -> u8 { .. } }   // live: main calls it
impl B { pub fn run(&self) -> u8 { .. } }   // dead: only a test calls it

fn main() { let _ = A.run(); }
#[cfg(test)] mod tests { #[test] fn t() { let _ = B.run(); } }
```

```
default:  "run" is ambiguous — nothing reported          (false negative)
precise:  B::run — 2 functions, confident                (found)
```

## Edge coverage: what was wrong, and what still is

The tier is validated on controlled cases — same-named methods, same-named
free functions in different modules, a name shared between a test and
production, production versus test-only reachability. Each is a regression
test, and each fails without it.

It nonetheless over-reported badly on real crates, and this document
previously said the cause was "a call form the parser does not recognise".
That was the right kind of answer and the wrong one in fact. Measuring it —
counting call terminators in a real dump against the ones the parser
attributed — put edge loss at **47.8% on one crate and 25% on another**, and
named three specific defects, none of them a call form:

**The impl block is printed as a source span.** A method's header reads
`config::<impl at src/config.rs:35:1: 35:12>::is_root`, and the parser
recognised that form only as a *prefix*, never module-qualified. On one crate
296 of 506 headers looked like this. Each was rejected, attribution stopped,
and every call in the body was discarded. The receiver type MIR prints in the
parameter list — `_1: &Config` — is the identity a call site would name, and
is now used.

**A closure is not reliably printed after its parent.** Resolving a parent
against what had been seen so far therefore lost every closure printed first.
Parsing is now two-pass: collect identities, then attribute.

**A call to a function returning `!` has no return successor.** MIR writes
`_1 = run_stdio() -> unwind continue;` with no successor list, and the parser
required one. A `main` whose body is one call to a never-returning run loop
produces exactly that line and nothing else — so on one crate the entire
binary was unreachable, and with it every function the program actually runs.

Together these recovered **97% more edges on one crate and 25% on another**.

## Partial MIR is refused

`cargo rustc` passes trailing arguments to one target, so a workspace is
dumped a target at a time. If a target failed to compile, its failure used to
be discarded as long as some other target succeeded.

That is the worst available outcome. Definitions come from source, so every
function in the failed target keeps its definition and loses every call it
makes; everything those calls reached is then reported unreachable, and
nothing in the output says which half of the graph is missing. Precise mode
now refuses, and names the targets that did not build.

The same reasoning already governs the mode's refusal to fall back at all.

## Where it stands now

| Crate | functions | ambiguous, default | ambiguous, precise | findings, default | findings, precise |
|---|---:|---:|---:|---:|---:|
| this one | 148 | 41 (27.7%) | 9 (6.1%) | 0 | 0 |
| F | 848 | 196 (23.1%) | 118 (13.9%) | 20 | 33 |
| G | 27 | 4 | 4 | 14 | 21 |
| H | 48 | 2 | 2 | 0 | 0 |

**On this crate precise now reports nothing.** It previously reported two, and
both were false: each was called only from inside a closure, and the closure's
calls were being dropped.

**On G every addition was checked against the source and every one was real.**
Two are methods with no caller of any kind; the rest are one module reachable
only from its own test functions and an integration test. The default tier
missed them because their names were ambiguous.

**On F the additions are not all verified.** Four that were checked — a `main`,
the run loop it calls, and two parsers below that — were false and are now
fixed. The remaining thirteen have not been read one by one, so the honest
statement is that the tier is much better and not yet proven.

Ambiguity is what the tier is for, and it now settles most of it: 27.7% to
6.1% on this crate, 23.1% to 13.9% on the largest. An earlier version of this
document claimed 52% and 62% from a matching rule that was unsound; those
numbers were withdrawn, and these come from a rule that promotes a definition
only when MIR's qualifier corresponds to metadata `syn` independently
recorded.

## Known limits

- **`no_std` crates cannot be analysed.** The mode compiles with `--profile
  test`, which links std's panic handler and collides with the crate's own:
  `found duplicate lang item 'panic_impl'`. The test profile is not optional —
  it is the only way MIR sees a `#[cfg(test)]` body — so this is a real
  exclusion rather than a bug to fix. The mode reports it and refuses.
- **A large increase over the default tier is still a warning sign**, and the
  report says so. Gate CI on the default tier; use `--precise` to investigate
  what the default declined to judge, and confirm each finding with
  `--explain`.

```
CAUTION — precise mode reports 33 findings; the default reports 20.
  ... Treat the additions as leads, verify with --explain, and trust
  the default tier where the two disagree.
```

`summary.nominal_findings` carries the same comparison in JSON.

## How it works

- **Definitions** come from `syn`. MIR carries no visibility, no
  `#[cfg(test)]`, no `#[allow(dead_code)]`, and no source line for a free
  function. Both frontends run; neither replaces the other.
- **Edges** come from MIR entirely. Keeping any syntactic edge would
  reintroduce the ambiguity the mode exists to remove.
- **Test context** comes from the caller: an edge out of a function `syn`
  recorded as test code is a test edge. MIR has no notion of test code, and
  `syn` read the attributes.
- **The test profile** is used, not dev or release. Release inlines calls away
  — `scope 3 (inlined A::process)` instead of a call terminator — and the call
  graph vanishes into the annotations. The test profile additionally compiles
  `#[cfg(test)]` bodies, without which a test-only call would not be seen at
  the same typed identity as a production one.
- **One target at a time.** `cargo rustc` passes trailing arguments to exactly
  one target, so a crate with a lib and a bin must be dumped per target and
  concatenated. Asking for all at once fails with a message about argument
  passing that says nothing about the code.

## Cost

A full debug build of the crate, plus MIR emission. Seconds for a small crate,
minutes for a large workspace. The default tier remains parse-only and does
not touch any of this.

## Stability

`-Zunpretty=mir` is explicitly not a stable interface — the dump begins
*"This output format is intended for human consumers only and is subject to
change without notice."* The parser is written to fail closed: an unrecognised
line yields no edge rather than a wrong one, and a dump with no `fn ` at all is
an error rather than an empty result that would report the whole crate dead.
