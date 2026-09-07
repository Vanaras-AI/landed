#!/usr/bin/env python3
"""Does `landed impact` ever omit a test that catches a bug?

Test selection is only worth having if it never drops a test that would have
failed. That is not a property you can argue for. You break a function, run
the whole suite, and check that every test which failed was predicted.

For each sampled function this:
  1. asks `landed impact --symbol <fn>` which tests it claims are affected,
  2. injects a fault as the function's first statement,
  3. runs the entire suite, without stopping at the first failure,
  4. checks every test that failed was in the predicted set.

A predicted set larger than the failing set is fine and expected: running a
test that did not need to run costs seconds. A test that failed and was NOT
predicted is a regression that would have reached production, and is reported
as UNSOUND.

Two numbers decide whether test selection is worth building on:

  miss rate       must be zero. Anything else and selection cannot ship.
  opaque fraction how much of the suite can never be skipped, because it
                  reaches the program through a boundary no call edge
                  crosses. `landed impact <repo>` reports it on its own.
                  This is a property of the suite; no amount of better
                  analysis moves it.

Usage:
  scripts/soundness.py [REPO] [--n 10] [--seed 0] [--test-cmd "..."] [--lang L]
"""
import argparse, json, os, random, re, shlex, subprocess, sys, pathlib

MARK = "landed-soundness-mutant"


class Lang:
    """How to find functions, break one, and read the failures back."""

    def __init__(self, name, globs, test_cmd, fn_re, fault, fail_re):
        self.name, self.globs, self.test_cmd = name, globs, test_cmd
        self.fn_re, self.fault, self.fail_re = fn_re, fault, fail_re


LANGS = {
    "rust": Lang(
        "rust", ["**/*.rs"],
        "cargo test --release --no-fail-fast",
        r"^([ \t]*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn ([a-z_][A-Za-z0-9_]*)\s*[(<]",
        'panic!("%s");' % MARK,
        r"^test (\S+) \.\.\. FAILED",
    ),
    "python": Lang(
        "python", ["**/*.py"],
        "python3 -m pytest -q",
        r"^([ \t]*)def ([a-z_][A-Za-z0-9_]*)\s*\(",
        'raise RuntimeError("%s")' % MARK,
        # pytest and unittest both, since a repo may use either.
        r"^(?:FAILED|ERROR) \S+::(\S+)|^(?:FAIL|ERROR): (\w+)",
    ),
    "go": Lang(
        "go", ["**/*.go"],
        "go test ./...",
        r"^()func (?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*\(",
        'panic("%s")' % MARK,
        r"^\s*--- FAIL: (\S+)",
    ),
    "typescript": Lang(
        "typescript", ["**/*.ts", "**/*.tsx", "**/*.js"],
        "npx vitest run --reporter=verbose",
        r"^([ \t]*)(?:export\s+)?(?:async\s+)?function ([A-Za-z_][A-Za-z0-9_]*)\s*[(<]",
        'throw new Error("%s");' % MARK,
        r"(?:×|FAIL)\s+\S+\s*>\s*(.+?)\s*$",
    ),
}


def detect(repo):
    for name, marker in (
        ("rust", "Cargo.toml"),
        ("go", "go.mod"),
        ("python", "pyproject.toml"),
        ("typescript", "package.json"),
    ):
        if (repo / marker).exists():
            return name
    return None


def sh(cmd, repo, **kw):
    if isinstance(cmd, str):
        cmd = shlex.split(cmd)
    return subprocess.run(cmd, cwd=repo, capture_output=True, text=True, **kw)


def landed_bin():
    here = pathlib.Path(__file__).resolve().parent.parent / "target" / "release" / "landed"
    return str(here) if here.exists() else "landed"


def predicted(repo, fn):
    r = sh([landed_bin(), "impact", str(repo), "--symbol", fn, "--json"], repo)
    if not r.stdout.strip():
        return None
    try:
        return json.loads(r.stdout)
    except json.JSONDecodeError:
        return None


def sources(repo, lang):
    skip = ("/target/", "/node_modules/", "/.git/", "/vendor/", "/dist/", "/build/", "/.venv/")
    out = []
    for g in lang.globs:
        for p in repo.glob(g):
            s = str(p)
            if not any(k in s for k in skip):
                out.append(p)
    return out


def functions(repo, lang):
    found = []
    for p in sources(repo, lang):
        try:
            text = p.read_text(errors="ignore")
        except OSError:
            continue
        for m in re.finditer(lang.fn_re, text, re.M):
            found.append((p, m.group(2)))
    return sorted(set(found), key=lambda x: (str(x[0]), x[1]))


def mutate(path, fn, lang):
    """Insert a fault as the function's first statement. Returns the original."""
    original = path.read_text(errors="ignore")
    m = re.search(lang.fn_re.replace("([a-z_][A-Za-z0-9_]*)", re.escape(fn))
                            .replace("([A-Za-z_][A-Za-z0-9_]*)", re.escape(fn)),
                  original, re.M)
    if not m:
        return None
    indent = m.group(1) or ""
    if lang.name == "python":
        eol = original.find("\n", m.end())
        colon = original.find(":", m.end())
        if colon < 0 or (0 <= eol < colon):
            return None  # multi-line signature: skip rather than guess
        insert_at = original.find("\n", colon) + 1
        body_indent = indent + "    "
        injected = original[:insert_at] + f"{body_indent}{lang.fault}\n" + original[insert_at:]
    else:
        brace = original.find("{", m.end())
        if brace < 0:
            return None
        injected = original[: brace + 1] + f"\n{indent}    {lang.fault}\n" + original[brace + 1 :]
    path.write_text(injected)
    return original


def failing(repo, lang, cmd):
    r = sh(cmd, repo)
    out = r.stdout + r.stderr
    names = set()
    for m in re.finditer(lang.fail_re, out, re.M):
        # A runner may be matched by any one of several alternations.
        name = next((g for g in m.groups() if g), None)
        if name:
            names.add(name.split("::")[-1].strip())
    return names


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("repo", nargs="?", default=".")
    ap.add_argument("--n", type=int, default=10)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--lang")
    ap.add_argument("--test-cmd")
    args = ap.parse_args()

    repo = pathlib.Path(args.repo).resolve()
    name = args.lang or detect(repo)
    if name not in LANGS:
        print(f"cannot tell what {repo} is written in; pass --lang", file=sys.stderr)
        return 2
    lang = LANGS[name]
    cmd = args.test_cmd or lang.test_cmd

    profile = sh([landed_bin(), "impact", str(repo)], repo).stdout
    print(profile.strip())
    print()
    print(f"suite command: {cmd}")
    print()

    pool = functions(repo, lang)
    random.seed(args.seed)
    random.shuffle(pool)

    checked = unsound = 0
    for path, fn in pool:
        if checked >= args.n:
            break
        pred = predicted(repo, fn)
        if not pred or not pred.get("changed_symbols"):
            continue
        original = mutate(path, fn, lang)
        if original is None:
            continue
        try:
            failed = failing(repo, lang, cmd)
        finally:
            path.write_text(original)
        if not failed:
            continue  # the fault changed nothing observable; proves nothing
        checked += 1
        missed = sorted(failed - set(pred["tests"]))
        unsound += bool(missed)
        print(
            f"{'UNSOUND' if missed else 'ok':>8}  {fn:<34} "
            f"predicted {pred['tests_affected']:>4}  failed {len(failed):>4}"
            + (f"  MISSED {missed[:6]}" if missed else "")
        )

    print()
    print(f"functions checked : {checked}")
    print(f"miss rate         : {unsound}/{checked}" if checked else "nothing was checked")
    print()
    if checked and unsound == 0:
        print("SOUND — no test that failed went unpredicted.")
        print("Read the opaque fraction above to see whether selection is worth it here.")
    elif checked:
        print(f"UNSOUND on {unsound}/{checked}. Do not ship selection on this codebase.")
    return 1 if unsound else 0


if __name__ == "__main__":
    sys.exit(main())
