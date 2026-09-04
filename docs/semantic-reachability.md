# Semantic reachability

Structural reachability answers *can control flow arrive here*. It cannot
answer *can it arrive here with values that do anything*, and that is where
the original motivating bug lives:

```rust
// The feature.
if trace.confidence >= 0.85 { promote(trace); }

// Production, another file, months earlier.
SkillTrace { confidence: 0.0, .. }   // every construction site
```

`promote` is structurally reachable — there is a call, in live code, on a live
path. It is *behaviourally* unreachable: no production value can satisfy the
guard. The current analyzer is silent here and will stay silent no matter how
good its call graph gets, because the question is about values, not edges.

## What it needs that does not exist yet

Per-**type** field tracking. `confidence` appears on four different structs in
the codebase that motivated this; a name-keyed store cannot tell them apart,
so the analysis would conflate `SkillTrace.confidence` with
`MemoryRecord.confidence` and conclude nothing useful. This work is therefore
**blocked on the resolved-symbol IR** — specifically on `SymbolId.self_ty`
being populated, which means the MIR tier from `docs/resolution-spike.md`.

Concretely, the minimum:

| Fact | Why | Available from |
|---|---|---|
| Field identity — `(Type, field)`, not `field` | four structs share the name `confidence` | MIR / HIR |
| Every construction site of that type | a single missed one invalidates the conclusion | MIR |
| The literal or constant written at each | that is the value being propagated | MIR / syn |
| Whether any write is non-constant | one parameterised write means unknown, not zero | MIR |
| The comparison and its constant | `>= 0.85` | MIR / syn |
| Whether the type crosses a crate boundary | a downstream crate may write any value | cargo metadata + visibility |

That last row is the soundness hazard the current tool already respects
elsewhere: `SkillTrace` is `pub` with `pub` fields, so a consumer could
construct one with `confidence: 0.9`. Within a binary that cannot happen;
within a library it can, and the analysis must decline.

## The order to build it, and where to stop

Each stage is independently useful and independently wrong in a bounded way.
Do not start with symbolic execution.

**1 — Constants.** `const X: f64 = 0.0;` compared against a literal. Trivial,
and catches a real class: a threshold guarded by a constant nobody updated.

**2 — Literal field writes, whole-crate.** For a private type, join the
literals written to a field at every construction site. If the join is a
single constant, evaluate every comparison against it. This is the
`confidence: 0.0` case exactly.

**3 — `Default` and builders.** `..Default::default()` is a construction site
that names no field, so the `Default` impl must be resolved and read. A
builder whose setter takes a parameter is an unknown write, and one unknown
write must collapse the whole analysis for that field to `Top`.

**4 — Simple propagation.** `let c = trace.confidence;` then `if c >= 0.85`.
One level of local aliasing, no further.

**5 — Stop.** Interprocedural data flow, loops, and path conditions are a
different project with a different failure mode. The value here is in the
narrow, checkable cases; a half-built symbolic executor that is wrong in ways
nobody can predict is worse than silence.

## The lattice, and the direction it must fail

```
        Top  (unknown — any value possible)
         │
    Const(v)  (every write agrees on v)
         │
       Bottom (no write seen at all)
```

Join is the usual: two different constants meet at `Top`. Any write the
analyzer cannot read — a function call, a parameter, a deserialiser, an FFI
boundary, a field never written in this crate — is `Top`, not `Bottom`.

That distinction is the whole safety argument. `Bottom` would let an
unanalysable field read as "never written", and the analyzer would confidently
report a live feature as behaviourally dead. `Top` makes it silent instead.
This is the same rule the existing analysis follows — *an approximation must
be able to suppress a finding, never to create one* — applied to values rather
than edges.

## What a finding looks like

Nothing about this is worth shipping without the evidence attached, because
the claim is much stronger than "nothing calls this" and correspondingly
harder to believe:

```
promote()
  src/promotion.rs:42

  status       BEHAVIOURALLY UNREACHABLE
  confidence   High

  guard        trace.confidence >= 0.85
               src/promotion.rs:41

  writes       SkillTrace.confidence — 2 production construction sites,
               both literal 0.0
                 src/skills/mod.rs:433
                 src/skills/mod.rs:619

  test writes  0.90, 0.95, 0.88  (src/skills/mod.rs:639)

  visibility   SkillTrace is crate-private; no external construction possible

  conclusion   the guard cannot hold on any value production constructs.
               The tests satisfy it using a helper that manufactures a state
               production does not produce.
```

The `visibility` line is not decoration — it is the premise the whole
conclusion rests on, and a reader must be able to check it.

## Confidence, and why this tier needs its own

A behavioural finding is a stronger claim than a structural one and fails
differently: a missed construction site does not merely lose a finding, it
produces a false one. It therefore does not inherit the existing
`High`/`Medium`, which are about *edge* resolution:

- **High** — private type, every construction site read, all literal, single
  constant.
- **Medium** — public type in a binary crate, or a `Default` impl resolved
  transitively.
- **Never reported** — any `Top` write, any public type in a library, any
  construction the analyzer could not read.

## Why this is last

It is the most valuable feature on the list and the one most likely to
discredit the tool if rushed. Behavioural unreachability is the thing nobody
else reports; it is also the thing where a single wrong answer costs more than
ten missed ones, because the user has no cheap way to check it — unlike "no
caller exists", which is one grep.

Build it after the IR, after MIR, and after there is a corpus of real cases to
regress against. There is one already: the codebase that motivated it.
