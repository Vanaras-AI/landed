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

## Known limitation: edge coverage on large crates

The tier is validated on controlled cases — same-named methods, same-named
free functions in different modules, a name shared between a test and
production, production versus test-only reachability. Each is a regression
test, and each fails without it.

**On large real crates it currently over-reports.** Measured findings, default
versus precise:

| Crate | functions | default | precise |
|---|---:|---:|---:|
| A | 158 | 0 | 10 |
| B | 445 | 1 | 18 |
| C | 667 | 15 | 101 |
| D | 1004 | 16 | 49 |

The cause is edge coverage, not identity. This tier reads a dump the compiler
prints for humans, and a call form the parser does not recognise is a call it
does not see; everything that call reached then looks unreachable. Closure
bodies were one such form and are now attributed to the function that wrote
them; there are evidently others.

The mode says so rather than presenting the number as fact:

```
CAUTION — precise mode reports 101 findings; the default reports 15.
  This tier resolves identity from the compiler's human-readable MIR
  dump. A call form its parser does not recognise is a call it does not
  see ... Treat the additions as leads, verify with --explain, and trust
  the default tier where the two disagree.
```

`summary.nominal_findings` carries the same comparison in JSON.

**Until edge coverage is closed, `--precise` is a lead-generator for
ambiguous symbols, not a CI gate.** Gate on the default tier; use `--precise`
to investigate what the default declined to judge, and confirm each with
`--explain`.

## What it measurably does, and does not

Every column from the current build, on real crates:

| Crate | functions | ambiguous, default | ambiguous, precise | findings, default | findings, precise |
|---|---:|---:|---:|---:|---:|
| this one | 105 | 16 (15.2%) | 14 (13.3%) | 0 | 2 |
| A | 158 | 41 (25.9%) | 41 (25.9%) | 0 | 10 |
| E | 155 | 54 (34.8%) | 54 (34.8%) | 1 | 6 |
| B | 445 | 127 (28.5%) | 127 (28.5%) | 1 | 18 |
| C | 667 | 78 (11.7%) | 76 (11.4%) | 15 | 101 |
| D | 1004 | 314 (31.3%) | 294 (29.3%) | 16 | 49 |

Crates are lettered rather than named; the same letter means the same crate in
both tables.

**Ambiguity falls only slightly.** Identity promotion requires MIR's qualifier
to match metadata `syn` independently recorded — a receiver type, or a module
path that MIR's qualifier is a suffix of. Where that correspondence cannot be
established the definition is left nominal, because promoting on a guess would
key a definition to something no call site reaches, and strand it.

An earlier, looser rule reported far larger reductions (52%, 62%). It was
matching on name and receiver alone, and the reductions were not sound; they
are not reproduced here because the rule that produced them was replaced.

The gap this tier was built to close is **not closed**. On these crates it
settles a few percent of it. What it does reliably is distinguish specific
same-named symbols once identity is established, which the regression cases
demonstrate and which the nominal tier cannot do at all.

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
