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

## What it measurably does, and does not

Ambiguity before and after, on real crates:

| Crate | default | precise | reduction |
|---|---:|---:|---:|
| hyperfine | 34.8% | 16.8% | 52% |
| landed | 16.5% | 6.2% | 62% |
| vibeEmu | 31.3% | 22.9% | 27% |
| fd | 25.9% | 21.5% | 17% |
| coda | 11.7% | 10.0% | 15% |

**Roughly a third of the gap on average, not all of it.** The MIR text dump
identifies a method by its receiver type — which is where most ambiguity lives
— but names a free function by bare identifier with no module path:

```
_7 = nested() -> [return: bb4, unwind continue];
```

Two `nested` functions in different modules stay indistinguishable, so the
tier reports `Precision::Typed`, not `Resolved`. The remaining ambiguity is
still reported by `--stats`; it is not hidden because a better frontend ran.

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
