//! The same analysis, in four languages.
//!
//! Reachability is a property of a call graph, not of a syntax. These tests
//! write the identical shape — a live helper, a helper only a test calls, and
//! a function reachable only through that one — in Rust, Python, TypeScript
//! and Go, and assert the analyzer reaches the identical conclusion.
//!
//! Each fixture is an application: a library has no unreachable public
//! surface by definition, so the interesting case needs an entry point.

use landed::lang::Language;
use landed::{project, scan};
use std::path::{Path, PathBuf};

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// A directory no other test can be using.
///
/// Keying on the name and the pid alone was not enough: two tests both asked
/// for "lib", ran in parallel, and each wiped the other's tree on the way in.
/// It passed alone and failed in the suite, which is the worst way for a test
/// to be wrong. The counter makes every call distinct.
fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("landed-lang-{name}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn dead_names(dir: &Path) -> Vec<String> {
    let s = scan::scan_crate(dir).unwrap();
    let mut v: Vec<String> = scan::never_run_graph(&s)
        .into_iter()
        .map(|f| f.name)
        .collect();
    v.sort();
    v
}

// ─── the four fixtures ────────────────────────────────────────

fn python(dir: &Path) {
    write(
        dir,
        "pkg/core.py",
        "def live_helper():\n    return 1\n\ndef dead_helper():\n    return deeper()\n\n\
         def deeper():\n    return 2\n\ndef run():\n    return live_helper()\n",
    );
    write(dir, "pkg/__main__.py", "from .core import run\n\nrun()\n");
    write(
        dir,
        "tests/test_core.py",
        "from pkg.core import dead_helper\n\ndef test_dead():\n    assert dead_helper() == 2\n",
    );
}

fn typescript(dir: &Path) {
    write(
        dir,
        "package.json",
        r#"{"name":"d","bin":{"d":"src/main.js"}}"#,
    );
    write(
        dir,
        "src/core.ts",
        "export function liveHelper(): number { return 1; }\n\
         export function deadHelper(): number { return deeper(); }\n\
         function deeper(): number { return 2; }\n\
         export function run(): number { return liveHelper(); }\n",
    );
    write(
        dir,
        "src/main.ts",
        "import { run } from \"./core\";\nrun();\n",
    );
    write(
        dir,
        "__tests__/core.test.ts",
        "import { deadHelper } from \"../src/core\";\n\
         test(\"dead\", () => { deadHelper(); });\n",
    );
}

fn go(dir: &Path) {
    write(dir, "go.mod", "module demo\n\ngo 1.21\n");
    write(
        dir,
        "main.go",
        "package main\n\nfunc liveHelper() int { return 1 }\n\
         func deadHelper() int { return deeper() }\n\
         func deeper() int { return 2 }\n\
         func run() int { return liveHelper() }\n\
         func main() { run() }\n",
    );
    write(
        dir,
        "main_test.go",
        "package main\n\nimport \"testing\"\n\nfunc TestDead(t *testing.T) { deadHelper() }\n",
    );
}

fn rust(dir: &Path) {
    write(
        dir,
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        dir,
        "src/main.rs",
        "fn live_helper() -> u8 { 1 }\n\
         pub fn dead_helper() -> u8 { deeper() }\n\
         fn deeper() -> u8 { 2 }\n\
         fn run() -> u8 { live_helper() }\n\
         fn main() { let _ = run(); }\n\
         #[cfg(test)]\n\
         mod tests { #[test] fn t() { let _ = super::dead_helper(); } }\n",
    );
}

// ─── the shared conclusion ────────────────────────────────────

/// `dead_helper` is entered only from a test; `deeper` sits behind it. Both
/// are unreachable. `live_helper` and `run` are on the path from the entry
/// point and must not be reported — a tool that flags live code is worse than
/// one that flags nothing.
fn assert_same_conclusion(dir: &Path, dead: &str, deeper: &str, live: &[&str]) {
    let names = dead_names(dir);
    assert!(
        names.contains(&dead.to_string()),
        "expected {dead} dead, got {names:?}"
    );
    assert!(
        names.contains(&deeper.to_string()),
        "expected {deeper} dead, got {names:?}"
    );
    for l in live {
        assert!(
            !names.contains(&l.to_string()),
            "{l} is live but was reported: {names:?}"
        );
    }
    assert_eq!(
        names.len(),
        2,
        "exactly two findings expected, got {names:?}"
    );
}

#[test]
fn python_finds_the_test_only_region() {
    let dir = scratch("py");
    python(&dir);
    assert_same_conclusion(&dir, "dead_helper", "deeper", &["live_helper", "run"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn typescript_finds_the_test_only_region() {
    let dir = scratch("ts");
    typescript(&dir);
    assert_same_conclusion(&dir, "deadHelper", "deeper", &["liveHelper", "run"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn go_finds_the_test_only_region() {
    let dir = scratch("go");
    go(&dir);
    assert_same_conclusion(&dir, "deadHelper", "deeper", &["liveHelper", "run", "main"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rust_finds_the_test_only_region() {
    let dir = scratch("rs");
    rust(&dir);
    assert_same_conclusion(
        &dir,
        "dead_helper",
        "deeper",
        &["live_helper", "run", "main"],
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ─── detection ────────────────────────────────────────────────

#[test]
fn a_manifest_names_the_language() {
    for (name, build, want) in [
        ("d-py", python as fn(&Path), Language::Python),
        ("d-ts", typescript as fn(&Path), Language::TypeScript),
        ("d-go", go as fn(&Path), Language::Go),
        ("d-rs", rust as fn(&Path), Language::Rust),
    ] {
        let dir = scratch(name);
        build(&dir);
        assert_eq!(
            landed::lang::detect(&dir),
            Some(want),
            "in {}",
            dir.display()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Python has no manifest here, so detection falls to file counts. A stray
/// script of another language must not change what the project is.
#[test]
fn a_stray_file_does_not_change_the_language() {
    let dir = scratch("stray");
    python(&dir);
    write(&dir, "tools/oneoff.go", "package main\nfunc main() {}\n");
    assert_eq!(landed::lang::detect(&dir), Some(Language::Python));
    std::fs::remove_dir_all(&dir).ok();
}

/// Detection is right for a project and wrong for a repository that holds
/// several. The override exists for that, and must beat the manifest.
#[test]
fn the_override_beats_detection() {
    let dir = scratch("override");
    go(&dir);
    write(
        &dir,
        "extra/util.py",
        "def only_python_sees_me():\n    return 1\n",
    );
    assert_eq!(landed::lang::detect(&dir), Some(Language::Go));

    let s = scan::scan_crate_as(
        &dir,
        landed::frontend::Tier::Default,
        Some(Language::Python),
    )
    .unwrap();
    let names: Vec<&str> = s.defs.iter().map(|d| d.name()).collect();
    assert!(names.contains(&"only_python_sees_me"), "got {names:?}");
    assert!(
        !names.contains(&"deadHelper"),
        "go code read as python: {names:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ─── project conventions ──────────────────────────────────────

#[test]
fn each_language_recognises_its_own_test_files() {
    for (name, build, tests, prod) in [
        (
            "t-py",
            python as fn(&Path),
            "tests/test_core.py",
            "pkg/core.py",
        ),
        (
            "t-ts",
            typescript as fn(&Path),
            "__tests__/core.test.ts",
            "src/core.ts",
        ),
        ("t-go", go as fn(&Path), "main_test.go", "main.go"),
        ("t-rs", rust as fn(&Path), "tests/it.rs", "src/main.rs"),
    ] {
        let dir = scratch(name);
        build(&dir);
        let p = project::detect(&dir);
        assert!(
            p.is_test_file(&dir.join(tests)),
            "{name}: {tests} should be test code"
        );
        assert!(
            !p.is_test_file(&dir.join(prod)),
            "{name}: {prod} should be production"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// A library has no entry point of its own, so its public surface is the root
/// set. An application's does not — that is what makes a finding possible.
#[test]
fn an_application_is_told_from_a_library() {
    let app = scratch("app");
    go(&app);
    assert!(project::detect(&app).is_application());

    let lib = scratch("lib");
    write(&lib, "go.mod", "module demo\n\ngo 1.21\n");
    write(
        &lib,
        "lib.go",
        "package demo\n\nfunc Exported() int { return 1 }\n",
    );
    assert!(!project::detect(&lib).is_application());

    std::fs::remove_dir_all(&app).ok();
    std::fs::remove_dir_all(&lib).ok();
}

/// Vendored trees are other people's code. Reporting it is noise, and reading
/// it is slow.
#[test]
fn vendored_directories_are_not_read() {
    let dir = scratch("vendor");
    python(&dir);
    write(
        &dir,
        "node_modules/dep/thing.py",
        "def vendored_and_unused():\n    return 1\n",
    );
    write(
        &dir,
        ".venv/lib/other.py",
        "def also_vendored():\n    return 1\n",
    );
    let names = dead_names(&dir);
    assert!(
        !names.contains(&"vendored_and_unused".to_string()),
        "got {names:?}"
    );
    assert!(
        !names.contains(&"also_vendored".to_string()),
        "got {names:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A test file runs at module level too. Crediting those calls to the same
/// root as a production module's would make anything a test file mentions
/// look reachable — which is exactly the finding this tool exists to make.
#[test]
fn a_module_level_call_in_a_test_file_is_not_a_production_entry_point() {
    let dir = scratch("modlevel");
    python(&dir);
    // No enclosing function at all: the call sits at the top level of a file
    // pytest collects.
    write(
        dir.as_path(),
        "tests/test_bare.py",
        "from pkg.core import dead_helper\n\ndead_helper()\n",
    );
    let names = dead_names(&dir);
    assert!(names.contains(&"dead_helper".to_string()), "got {names:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// Precision comes from reading a compiler's resolved output, and only the
/// Rust frontend does that. Asking for it elsewhere must say so rather than
/// quietly answer with less.
#[test]
fn precise_mode_refuses_rather_than_downgrades() {
    let dir = scratch("precise");
    go(&dir);
    let msg = match scan::scan_crate_as(&dir, landed::frontend::Tier::Precise, None) {
        Ok(_) => panic!("precise mode answered for a non-Rust project"),
        Err(e) => e.to_string(),
    };
    assert!(msg.contains("only available for Rust"), "got {msg}");
    assert!(
        msg.contains("go"),
        "the message should name what it found: {msg}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A scan that read nothing must not report it in the words of a clean result.
/// The wrong language on a polyglot tree is the common way to get here, and
/// "everything is reachable" would read as a pass.
#[test]
fn an_empty_scan_is_not_reported_as_a_clean_one() {
    let dir = scratch("empty");
    go(&dir);
    let s = scan::scan_crate_as(
        &dir,
        landed::frontend::Tier::Default,
        Some(Language::Python),
    )
    .unwrap();
    assert!(
        s.defs.is_empty(),
        "no python here, so nothing should have been read"
    );

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_landed"))
        .args([
            "check",
            dir.to_str().unwrap(),
            "--lang",
            "python",
            "--graph",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("No source was read"), "got {text}");
    assert!(!text.contains("Everything is reachable"), "got {text}");
    std::fs::remove_dir_all(&dir).ok();
}

// ─── TypeScript, as it is actually written ────────────────────

/// `const f = () => {}` is the dominant idiom in modern TypeScript and
/// carries no name of its own. Reading only `function` declarations found 5
/// callables in a 61-file extension and pronounced the rest of it clean.
#[test]
fn an_arrow_function_bound_to_a_name_is_a_definition() {
    let dir = scratch("arrow");
    write(&dir, "package.json", r#"{"name":"d","private":true}"#);
    write(&dir, "index.html", "<!doctype html>");
    write(
        &dir,
        "src/main.ts",
        "export const used = () => 1;\n\
         export const unused = () => 2;\n\
         const local = () => used();\n\
         local();\n",
    );
    write(
        &dir,
        "src/main.test.ts",
        "import { unused } from \"./main\";\ntest(\"u\", () => { unused(); });\n",
    );
    let s = scan::scan_crate(&dir).unwrap();
    let names: Vec<&str> = s.defs.iter().map(|d| d.name()).collect();
    for want in ["used", "unused", "local"] {
        assert!(names.contains(&want), "{want} not read: {names:?}");
    }
    assert!(dead_names(&dir).contains(&"unused".to_string()));
    std::fs::remove_dir_all(&dir).ok();
}

/// An anonymous callback has no name, so it is no definition — but the calls
/// in its body still belong to whoever wrote it.
#[test]
fn an_anonymous_callback_is_not_a_definition_but_its_body_still_counts() {
    let dir = scratch("cb");
    write(&dir, "package.json", r#"{"name":"d","private":true}"#);
    write(&dir, "index.html", "<!doctype html>");
    write(
        &dir,
        "src/main.ts",
        "const helper = (x: number) => x;\n\
         export const run = (xs: number[]) => xs.map((x) => helper(x));\n\
         run([1]);\n",
    );
    assert!(
        dead_names(&dir).is_empty(),
        "a call inside a callback still reaches its target: {:?}",
        dead_names(&dir)
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `<Chart />` is how a React component is used. It is not a call expression,
/// and a `.tsx` file needs the TSX grammar or everything after the first tag
/// is unread.
#[test]
fn a_component_used_only_in_jsx_is_reachable() {
    let dir = scratch("jsx");
    write(&dir, "package.json", r#"{"name":"d","private":true}"#);
    write(&dir, "index.html", "<!doctype html>");
    write(
        &dir,
        "src/Chart.tsx",
        "export const Chart = () => <div>chart</div>;\n\
         export const Unused = () => <div>nope</div>;\n",
    );
    write(
        &dir,
        "src/main.tsx",
        "import { Chart } from \"./Chart\";\n\
         const App = () => <div><Chart /></div>;\n\
         App();\n",
    );
    write(
        &dir,
        "src/Chart.test.tsx",
        "import { Chart, Unused } from \"./Chart\";\ntest(\"c\", () => { Chart(); Unused(); });\n",
    );
    let names = dead_names(&dir);
    assert!(
        !names.contains(&"Chart".to_string()),
        "JSX use missed: {names:?}"
    );
    assert!(names.contains(&"Unused".to_string()), "got {names:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A library's public surface is its root set, so what counts as public
/// decides whether the analysis says anything at all. `export` is the marker
/// in TypeScript; a leading underscore is not, and using that convention here
/// made every function a root.
#[test]
fn typescript_publicness_is_export_not_a_naming_convention() {
    let dir = scratch("exp");
    // No bin, no private, no index.html: a library, so exports are roots.
    write(&dir, "package.json", r#"{"name":"lib","main":"dist/i.js"}"#);
    write(
        &dir,
        "src/i.ts",
        "export const publicThing = () => 1;\n\
         const internalThing = () => 2;\n",
    );
    write(
        &dir,
        "src/i.test.ts",
        "test(\"i\", () => { publicThing(); internalThing(); });\n",
    );
    let names = dead_names(&dir);
    assert!(
        !names.contains(&"publicThing".to_string()),
        "exports are roots: {names:?}"
    );
    assert!(
        names.contains(&"internalThing".to_string()),
        "got {names:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Go states this in the language: a capitalised identifier is exported from
/// its package, and nothing else is.
#[test]
fn go_publicness_is_capitalisation() {
    let dir = scratch("gopub");
    write(&dir, "go.mod", "module demo\n\ngo 1.21\n");
    write(
        &dir,
        "lib.go",
        "package demo\n\nfunc Exported() int { return 1 }\n\
         func unexported() int { return 2 }\n",
    );
    write(
        &dir,
        "lib_test.go",
        "package demo\n\nimport \"testing\"\n\
         func TestBoth(t *testing.T) { Exported(); unexported() }\n",
    );
    let names = dead_names(&dir);
    assert!(!names.contains(&"Exported".to_string()), "got {names:?}");
    assert!(names.contains(&"unexported".to_string()), "got {names:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// An extension is entered by its host, at a name only the entry module
/// knows. Treating that module's exports as ordinary code condemned 107 live
/// functions behind a single unresolved root on a real extension.
#[test]
fn the_entry_module_a_host_loads_has_root_exports() {
    let dir = scratch("host");
    write(
        &dir,
        "package.json",
        r#"{"name":"ext","main":"./out/extension.js","engines":{"vscode":"^1.74.0"}}"#,
    );
    write(
        &dir,
        "src/extension.ts",
        "import { boot } from \"./boot\";\n\
         export const activate = () => boot();\n",
    );
    write(
        &dir,
        "src/boot.ts",
        "export const boot = () => 1;\n\
         export const neverBooted = () => 2;\n",
    );
    write(
        &dir,
        "src/boot.test.ts",
        "import { boot, neverBooted } from \"./boot\";\n\
         test(\"b\", () => { boot(); neverBooted(); });\n",
    );
    let names = dead_names(&dir);
    assert!(
        !names.contains(&"activate".to_string()),
        "host entry is a root: {names:?}"
    );
    assert!(
        !names.contains(&"boot".to_string()),
        "reached through activate: {names:?}"
    );
    assert!(names.contains(&"neverBooted".to_string()), "got {names:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A web app and an extension are applications, however they say so. Only a
/// package meant to be imported is a library, and calling one the other makes
/// the analysis either silent or wrong.
#[test]
fn a_typescript_application_is_told_from_a_library() {
    for (name, manifest, index, want_app) in [
        ("cli", r#"{"name":"a","bin":{"a":"i.js"}}"#, false, true),
        ("web", r#"{"name":"a","private":true}"#, true, true),
        (
            "ext",
            r#"{"name":"a","engines":{"vscode":"^1.0.0"}}"#,
            false,
            true,
        ),
        ("lib", r#"{"name":"a","main":"dist/i.js"}"#, false, false),
    ] {
        let dir = scratch(name);
        write(&dir, "package.json", manifest);
        if index {
            write(&dir, "index.html", "<!doctype html>");
        }
        write(&dir, "src/i.ts", "export const f = () => 1;\n");
        assert_eq!(
            project::detect(&dir).is_application(),
            want_app,
            "{name} classified wrong"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

// ─── what a 23-repository sweep of public code taught ─────────
//
// Every test below is a defect found by pointing the tool at real projects
// rather than at fixtures written in the subset it already read.

/// A library that also ships a command is still a library. Read the runnable
/// signals alone and the best-known Python web framework is an application —
/// which stops its public API being a root, and then reports its most-used
/// function as confidently dead.
#[test]
fn a_python_package_that_ships_a_cli_is_still_a_library() {
    let dir = scratch("pylib");
    write(
        &dir,
        "pyproject.toml",
        "[project]\nname = \"demo\"\nversion = \"1.0\"\n\n[project.scripts]\ndemo = \"pkg.cli:main\"\n",
    );
    write(&dir, "pkg/__init__.py", "from .core import render\n");
    write(&dir, "pkg/__main__.py", "from .cli import main\n\nmain()\n");
    write(&dir, "pkg/core.py", "def render():\n    return 1\n");
    write(&dir, "pkg/cli.py", "def main():\n    return 0\n");

    let p = project::detect(&dir);
    assert!(!p.is_application(), "a packaged distribution is a library");
    assert!(
        !dead_names(&dir).contains(&"render".to_string()),
        "an exported function must not be reported: {:?}",
        dead_names(&dir)
    );
}

/// `__all__` and a package's `__init__.py` name modules, classes and free
/// functions. They never name a method, so judging methods by that list marks
/// a whole decorator API private and reports it dead.
#[test]
fn a_method_of_an_exported_class_is_public() {
    let dir = scratch("pymethod");
    write(
        &dir,
        "pyproject.toml",
        "[project]\nname = \"d\"\nversion = \"1\"\n",
    );
    write(&dir, "pkg/__init__.py", "from .app import App\n");
    write(
        &dir,
        "pkg/app.py",
        "class App:\n    def run(self):\n        return 1\n\n    def _hidden(self):\n        return 2\n",
    );
    let names = dead_names(&dir);
    assert!(!names.contains(&"run".to_string()), "got {names:?}");
}

/// A dunder is called by the language, not by name: `__call__` runs when an
/// instance is applied. Its absence from the call graph proves nothing.
#[test]
fn python_dunders_are_not_reported() {
    let dir = scratch("pydunder");
    write(
        &dir,
        "pyproject.toml",
        "[project]\nname = \"d\"\nversion = \"1\"\n",
    );
    write(&dir, "pkg/__init__.py", "from .wsgi import App\n");
    write(
        &dir,
        "pkg/wsgi.py",
        "class App:\n    def __call__(self, environ, start_response):\n        return []\n",
    );
    let names = dead_names(&dir);
    assert!(!names.contains(&"__call__".to_string()), "got {names:?}");
}

/// JavaScript is read by the TypeScript frontend. A tool that silently reads
/// nothing from a JavaScript project is worse than one that refuses to try.
#[test]
fn javascript_is_read() {
    let dir = scratch("js");
    write(
        &dir,
        "package.json",
        r#"{"name":"d","bin":{"d":"src/main.js"}}"#,
    );
    write(
        &dir,
        "src/core.js",
        "export function liveHelper() { return 1; }\n\
         export function deadHelper() { return deeper(); }\n\
         function deeper() { return 2; }\n\
         export function run() { return liveHelper(); }\n",
    );
    write(
        &dir,
        "src/main.js",
        "import { run } from \"./core\";\nrun();\n",
    );
    write(
        &dir,
        "__tests__/core.test.js",
        "import { deadHelper } from \"../src/core\";\ntest(\"d\", () => { deadHelper(); });\n",
    );
    assert_same_conclusion(&dir, "deadHelper", "deeper", &["liveHelper", "run"]);
}

/// `"engines": {"node": ">=20"}` is a compatibility range, and `"private"` on
/// a monorepo root says only that the root is unpublished. Reading either as
/// "this is an application" reported half a published library dead.
#[test]
fn a_node_engine_and_private_do_not_make_an_application() {
    let dir = scratch("tslib");
    write(
        &dir,
        "package.json",
        r#"{"name":"d","private":true,"engines":{"node":">=20"},"main":"./src/index.ts"}"#,
    );
    write(
        &dir,
        "src/index.ts",
        "export function api(): number { return 1; }\n",
    );
    assert!(!project::detect(&dir).is_application());

    let ext = scratch("tsext");
    write(
        &ext,
        "package.json",
        r#"{"name":"e","engines":{"vscode":"^1.74.0"},"main":"./out/x.js"}"#,
    );
    write(&ext, "src/x.ts", "export function activate() {}\n");
    assert!(
        project::detect(&ext).is_application(),
        "a named host loads this and calls into it"
    );
}

/// A store library hands its API out as object properties. Neither form is a
/// call site, and both are unmistakably uses.
#[test]
fn functions_handed_out_as_object_properties_are_not_dead() {
    let dir = scratch("tsobj");
    write(
        &dir,
        "package.json",
        r#"{"name":"d","main":"./src/index.ts"}"#,
    );
    write(
        &dir,
        "src/index.ts",
        "function setState() { return 1; }\n\
         export function create() {\n\
         \x20 return { setState, clearStorage: () => 2 };\n\
         }\n",
    );
    let names = dead_names(&dir);
    assert!(
        !names.contains(&"setState".to_string()),
        "shorthand: {names:?}"
    );
    assert!(
        !names.contains(&"clearStorage".to_string()),
        "property: {names:?}"
    );
}

/// Go projects put shared test scaffolding in ordinary `.go` files so other
/// packages' tests can import it. Importing the standard `testing` package
/// settles what such a file is — it only works inside a test binary.
#[test]
fn a_go_file_importing_testing_is_test_code() {
    let dir = scratch("gotesting");
    go(&dir);
    write(
        &dir,
        "helpers.go",
        "package main\n\nimport \"testing\"\n\nfunc AssertThing(t *testing.T) { deadHelper() }\n",
    );
    let p = project::detect(&dir);
    assert!(p.is_test_file(&dir.join("helpers.go")));
    assert!(!p.is_test_file(&dir.join("main.go")));
}

/// Go interfaces are structural: a type satisfies one by having the methods,
/// with nothing written down to say so. An exported method may therefore be
/// reached through an interface no syntax reveals.
#[test]
fn an_exported_go_method_is_not_reported() {
    let dir = scratch("gomethod");
    go(&dir);
    write(
        &dir,
        "render.go",
        "package main\n\ntype R struct{}\n\n\
         func (r R) Render() int { return 1 }\n\
         func (r R) internal() int { return 2 }\n",
    );
    let names = dead_names(&dir);
    assert!(!names.contains(&"Render".to_string()), "got {names:?}");
}
