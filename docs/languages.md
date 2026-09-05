# Languages

`landed` is written in Rust and analyses Rust, Python, TypeScript and Go.

Reachability is a property of a call graph. Once the graph exists, nothing
about the question is language-specific: a root set, transitive closure, and
the functions outside it. So the analysis layer — `ir.rs`, `scan.rs`,
`report.rs`, `baseline.rs`, `config.rs` — contains no reference to any
language, and adding one must not require editing it.

## The two boundaries

Everything language-specific sits behind exactly two interfaces.

**`Frontend`** turns source into definitions and edges.

| Language | Frontend | Precision |
|---|---|---|
| Rust | `SynFrontend` | Nominal |
| Rust, `--precise` | `MirFrontend` | Typed |
| Python, TypeScript, Go | `TreeSitterFrontend` | Nominal |

**`Project`** answers what the analysis needs to know about the tree, and
nothing else: which files hold production code, which hold tests, whether
anything here runs on its own, and where it starts.

| | Rust | Python | TypeScript | Go |
|---|---|---|---|---|
| identified by | `Cargo.toml` | `pyproject.toml`, `setup.py` | `tsconfig.json`, `package.json` | `go.mod` |
| layout from | `cargo metadata` | convention | convention | convention |
| test files | `tests/`, `*_test.rs`, `#[cfg(test)]` | `test_*.py`, `*_test.py`, `tests/` | `*.test.ts`, `*.spec.ts`, `__tests__/` | `*_test.go` |
| runs on its own if | a `[[bin]]` target | a console script or a `__main__` | `"bin"` in package.json | `package main` |

That table is the whole of what a language costs. `CargoProject` asks cargo,
because cargo knows exactly which files it compiles and can name targets that
live nowhere the convention predicts. `ConventionProject` covers the rest,
because the other three languages have conventions their own tooling enforces.

## Detection

A manifest decides outright. Failing that, the extension with the most files
wins, so a project can hold a stray script of another language without becoming
that language. Ties break on declaration order rather than hash iteration —
the output of a tool that gates CI has to be reproducible.

Detection is right for a project and wrong for a repository holding several.
`--lang` states the answer instead:

```bash
landed check --graph --lang python ./services
```

## Why tree-sitter

One dependency, one walker, four grammars. What the analyzer needs from a tree
is small and nearly identical everywhere — where functions are defined, what
they call, which class a method belongs to — so the walker is parameterised by
a per-language description of node kinds rather than written four times.

It parses; it does not resolve. Precision is `Nominal`, the same tier as the
Rust default: two same-named methods are one node, and the analysis reports
that it cannot judge them rather than guessing. This is the same silence
`--stats` measures on Rust, and the same reason `--precise` exists.

Grammars are pinned against a tree-sitter runtime of `0.25`; the grammar crates
require ABI 15, and an older runtime fails to load them at all rather than
mis-parsing.

## What is not modelled

- **Dynamic dispatch.** A method reached only through an interface, a duck-typed
  call, or a registry lookup has no edge. This over-approximates deadness in
  principle, but the confidence split contains it: a function with production
  callers that merely look unreachable is reported as uncertain, never
  confident.
- **Calls on an expression.** `foo()` and `obj.foo()` name something.
  `handlers[k]()` and `(await f())()` do not, and no name is guessed at.
- **Re-export and import graphs.** A name is a name. Python's `from x import y
  as z` records `z` at the call site and `y` at the definition; the two do not
  meet. This shows up as an unresolved edge, which can only suppress a finding
  through the same fallback that credits every definition sharing a name.

Each of these fails in the direction the tool is built to fail in: **an
over-approximation may suppress a finding, never create one.**

## Test-file module level

Python and TypeScript test files execute at module level, the same as
production modules. Crediting those calls to the same root would make anything
a test file mentions look production-reachable — which is precisely the finding
this tool exists to make. A module-level call inside a test file is therefore
attributed to a distinct root, `TEST_MODULE_ROOT`, which belongs to the test
root set and not the production one.

This was a real bug, caught by the TypeScript fixture: `test("x", () => {
deadHelper() })` has no enclosing *function* node, so the call landed at module
level and the finding vanished.

## Adding a language

1. A variant in `Language`, with its extensions and manifests.
2. A `Spec` in `tree_sitter_frontend.rs` — the node kinds for functions, types
   and calls — plus the grammar crate.
3. The four conventions in `ConventionProject`.
4. A fixture in `tests/languages.rs` asserting the same conclusion the other
   languages reach on the same shape.

Nothing in `scan.rs` changes. If it needs to, the boundary is in the wrong
place.
