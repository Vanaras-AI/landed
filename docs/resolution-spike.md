# Resolution architecture spike

Measured on rustc 1.92.0, aarch64-apple-darwin, 2026-09-04.

The analyzer matches call edges by function name. `A::process` and
`B::process` are one node, so findings are suppressed whenever a name is not
unique — **38–43% of production functions in real codebases**. This spike
asks what it would cost to resolve symbols properly, and compares the three
candidate frontends on evidence rather than reputation.

## Result

| | ra_ap_hir | rustdoc JSON | MIR |
|---|---|---|---|
| Resolves **definitions** | yes | yes | yes |
| Resolves **call sites** | yes | **no** | yes |
| Needs the crate to compile | no | yes | yes |
| Needs nightly / `RUSTC_BOOTSTRAP` | no | **yes** | **yes** |
| Transitive crates added | **128** | 9 | 0 |
| Cold build of the dependency | **43 s** | 3 s | n/a |
| Build artefacts | **324 MB** | 35 MB | n/a |
| API stability | `0.0.x` | `format_version: 56` | unstable text |

## rustdoc JSON is eliminated

Not on cost — on capability. Every function item carries:

```
keys inside inner.function: ['generics', 'has_body', 'header', 'sig']
body/call information present: NONE
```

`has_body` is a boolean. The body is not emitted. rustdoc JSON gives resolved
*definitions* with stable ids, signatures and spans, but no call sites at all,
so **it cannot build a call graph**. It could only ever tell us how many
definitions share a name — which is what `--stats` already reports from syn.

## ra_ap_hir is capable but chases the toolchain

It is rust-analyzer's own resolution engine, so it resolves everything an IDE
resolves, tolerates code that does not compile, and needs no unstable flags
from the user.

The cost is version churn. Its MSRV moves faster than installed toolchains:

```
0.0.350  requires rustc 1.98
0.0.340  requires rustc 1.95
0.0.320  builds on rustc 1.92   <- newest usable here
```

Six minor versions behind the latest release, on a toolchain three months
old. Pinning to `ra_ap_*` means either chasing rustc upgrades or freezing on
an old resolution engine. Combined with `0.0.x` versioning — no API stability
promise, 25 vendored `ra-ap-rustc_*` crates — this is a standing maintenance
cost, not a one-time integration.

## MIR resolves calls exactly

Debug MIR, from `cargo rustc -- -Zunpretty=mir`:

```rust
fn main() -> () {
    _2 = A::process(move _3) -> [return: bb1, unwind continue];
    _4 = helper()            -> [return: bb2, unwind continue];
}
fn helper() -> () {
    _2 = B::process(move _3) -> [return: bb1, unwind continue];
}
```

Two same-named methods, distinguished at the call site by receiver type, with
the defining impl carrying its source span. This is precisely the edge
information the name-keyed graph lacks, and it costs no dependencies.

Three caveats:

- **Unstable flag.** `-Zunpretty` needs nightly, or `RUSTC_BOOTSTRAP=1` on
  stable. Shipping a tool whose default path sets `RUSTC_BOOTSTRAP` is not
  acceptable: it silently opts the user's build into unstable behaviour.
- **The crate must compile**, and be built. syn analyses broken or partial
  code; MIR cannot.
- **Release mode inlines calls away** — `scope 3 (inlined A::process)` rather
  than a call terminator. The analysis must run on a debug profile.
- The textual form is not a stable interface. Parsing it will break.

## Recommendation: two tiers, not a migration

Do not replace syn. Add MIR beside it.

| Tier | Frontend | Requires | Confidence |
|---|---|---|---|
| default | syn | nothing | `High` / `Medium` as today |
| `--precise` | MIR | nightly, a successful build | `Certain` |

This fits the confidence model already in place rather than fighting it. The
default stays instant, dependency-free, and able to read code that does not
build. The precise tier answers the 38–43% the default declines to judge, and
its findings carry a confidence the default cannot offer.

It also fails safe. If nightly is absent or the build is broken, `--precise`
degrades to the default rather than producing nothing.

`ra_ap_hir` is the fallback if the MIR text format proves too unstable to
parse — capable, and the only option that works on non-compiling code, but
128 crates and a standing MSRV chase is a large bill for a tool whose main
virtue is that it is small.

## What this means for the IR

Both tiers must produce the same shape, or the frontend leaks into every
consumer. See `docs/symbol-ir.md`: the analysis layer must never learn which
frontend produced an edge, only how much that edge can be trusted.
