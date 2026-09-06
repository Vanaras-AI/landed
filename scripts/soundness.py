#!/usr/bin/env python3
"""Does `landed impact` ever omit a test that catches a bug?

Test selection is only worth having if it never drops a test that would have
failed. That is not a property you can argue for: you break a function, run
the whole suite, and check the tests that failed were all predicted.

For each sampled function this:
  1. asks `landed impact --symbol <fn>` which tests it claims are affected,
  2. injects a panic as the function's first statement,
  3. runs the entire suite with --no-fail-fast,
  4. checks every test that failed was in the predicted set.

A predicted set larger than the failing set is fine and expected — running a
test that did not need to run costs seconds. A failing test that was NOT
predicted is a released regression, and is reported as UNSOUND.

Usage:  python3 scripts/soundness.py [--n 10] [--seed 0]
"""
import argparse, json, random, re, subprocess, sys, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
LANDED = ROOT / "target" / "release" / "landed"


def run(cmd, **kw):
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, **kw)


def predicted(fn):
    r = run([str(LANDED), "impact", ".", "--symbol", fn, "--json"])
    if r.returncode not in (0, 2) or not r.stdout.strip():
        return None
    return json.loads(r.stdout)


def candidates():
    """Functions defined in src/ that at least one test claims to reach."""
    r = run([str(LANDED), "check", ".", "--json"])
    names = set()
    for path in (ROOT / "src").rglob("*.rs"):
        text = path.read_text()
        for m in re.finditer(r"^\s*(?:pub(?:\([^)]*\))?\s+)?fn ([a-z_][a-z0-9_]*)\s*[(<]", text, re.M):
            names.add((str(path.relative_to(ROOT)), m.group(1), m.start()))
    return sorted(names)


def mutate(relpath, fn):
    """Insert a panic as the first statement of `fn`. Returns the original text."""
    p = ROOT / relpath
    original = p.read_text()
    m = re.search(r"^\s*(?:pub(?:\([^)]*\))?\s+)?fn %s\s*[(<]" % re.escape(fn), original, re.M)
    if not m:
        return None
    brace = original.find("{", m.end())
    if brace < 0:
        return None
    injected = original[: brace + 1] + '\n    panic!("landed-soundness-mutant");\n' + original[brace + 1 :]
    p.write_text(injected)
    return original


def failing_tests():
    run(["cargo", "build", "--release"])
    r = run(["cargo", "test", "--release", "--no-fail-fast"])
    out = r.stdout + r.stderr
    return {
        m.group(1).split("::")[-1]
        for m in re.finditer(r"^test (\S+) \.\.\. FAILED", out, re.M)
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=10)
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args()

    random.seed(args.seed)
    pool = candidates()
    random.shuffle(pool)

    checked = unsound = 0
    total_pred = total_fail = 0
    report = []

    for relpath, fn, _ in pool:
        if checked >= args.n:
            break
        pred = predicted(fn)
        if pred is None or not pred["changed_symbols"]:
            continue
        original = mutate(relpath, fn)
        if original is None:
            continue
        try:
            failed = failing_tests()
        finally:
            (ROOT / relpath).write_text(original)
        if not failed:
            # The mutation changed nothing observable; it proves nothing.
            continue
        checked += 1
        missed = sorted(failed - set(pred["tests"]))
        total_pred += pred["tests_affected"]
        total_fail += len(failed)
        if missed:
            unsound += 1
        report.append((fn, pred["tests_affected"], len(failed), missed))
        print(
            f"{'UNSOUND' if missed else 'ok':>8}  {fn:<34} "
            f"predicted {pred['tests_affected']:>3}  failed {len(failed):>3}"
            + (f"  MISSED {missed}" if missed else "")
        )

    run(["cargo", "build", "--release"])
    print()
    print(f"functions checked : {checked}")
    print(f"unsound           : {unsound}")
    if checked:
        avg = 100.0 * (1 - total_pred / (checked * max(1, json.loads(run([str(LANDED),'impact','.','--symbol','main','--json']).stdout)['tests_total'])))
        print(f"mean reduction    : {avg:.1f}%")
    print()
    print("SOUND" if unsound == 0 else f"UNSOUND on {unsound}/{checked}")
    return 0 if unsound == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
