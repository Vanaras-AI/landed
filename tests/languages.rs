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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("landed-lang-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn dead_names(dir: &Path) -> Vec<String> {
    let s = scan::scan_crate(dir).unwrap();
    let mut v: Vec<String> = scan::never_run_graph(&s).into_iter().map(|f| f.name).collect();
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
    write(dir, "package.json", r#"{"name":"d","bin":{"d":"src/main.js"}}"#);
    write(
        dir,
        "src/core.ts",
        "export function liveHelper(): number { return 1; }\n\
         export function deadHelper(): number { return deeper(); }\n\
         function deeper(): number { return 2; }\n\
         export function run(): number { return liveHelper(); }\n",
    );
    write(dir, "src/main.ts", "import { run } from \"./core\";\nrun();\n");
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
    assert!(names.contains(&dead.to_string()), "expected {dead} dead, got {names:?}");
    assert!(names.contains(&deeper.to_string()), "expected {deeper} dead, got {names:?}");
    for l in live {
        assert!(!names.contains(&l.to_string()), "{l} is live but was reported: {names:?}");
    }
    assert_eq!(names.len(), 2, "exactly two findings expected, got {names:?}");
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
    assert_same_conclusion(&dir, "dead_helper", "deeper", &["live_helper", "run", "main"]);
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
        assert_eq!(landed::lang::detect(&dir), Some(want), "in {}", dir.display());
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
    write(&dir, "extra/util.py", "def only_python_sees_me():\n    return 1\n");
    assert_eq!(landed::lang::detect(&dir), Some(Language::Go));

    let s = scan::scan_crate_as(&dir, landed::frontend::Tier::Default, Some(Language::Python))
        .unwrap();
    let names: Vec<&str> = s.defs.iter().map(|d| d.name()).collect();
    assert!(names.contains(&"only_python_sees_me"), "got {names:?}");
    assert!(!names.contains(&"deadHelper"), "go code read as python: {names:?}");
    std::fs::remove_dir_all(&dir).ok();
}

// ─── project conventions ──────────────────────────────────────

#[test]
fn each_language_recognises_its_own_test_files() {
    for (name, build, tests, prod) in [
        ("t-py", python as fn(&Path), "tests/test_core.py", "pkg/core.py"),
        ("t-ts", typescript as fn(&Path), "__tests__/core.test.ts", "src/core.ts"),
        ("t-go", go as fn(&Path), "main_test.go", "main.go"),
        ("t-rs", rust as fn(&Path), "tests/it.rs", "src/main.rs"),
    ] {
        let dir = scratch(name);
        build(&dir);
        let p = project::detect(&dir);
        assert!(p.is_test_file(&dir.join(tests)), "{name}: {tests} should be test code");
        assert!(!p.is_test_file(&dir.join(prod)), "{name}: {prod} should be production");
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
    write(&lib, "lib.go", "package demo\n\nfunc Exported() int { return 1 }\n");
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
    write(&dir, "node_modules/dep/thing.py", "def vendored_and_unused():\n    return 1\n");
    write(&dir, ".venv/lib/other.py", "def also_vendored():\n    return 1\n");
    let names = dead_names(&dir);
    assert!(!names.contains(&"vendored_and_unused".to_string()), "got {names:?}");
    assert!(!names.contains(&"also_vendored".to_string()), "got {names:?}");
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
    write(dir.as_path(), "tests/test_bare.py", "from pkg.core import dead_helper\n\ndead_helper()\n");
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
    assert!(msg.contains("go"), "the message should name what it found: {msg}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A scan that read nothing must not report it in the words of a clean result.
/// The wrong language on a polyglot tree is the common way to get here, and
/// "everything is reachable" would read as a pass.
#[test]
fn an_empty_scan_is_not_reported_as_a_clean_one() {
    let dir = scratch("empty");
    go(&dir);
    let s = scan::scan_crate_as(&dir, landed::frontend::Tier::Default, Some(Language::Python))
        .unwrap();
    assert!(s.defs.is_empty(), "no python here, so nothing should have been read");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_landed"))
        .args(["check", dir.to_str().unwrap(), "--lang", "python", "--graph"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("No source was read"), "got {text}");
    assert!(!text.contains("Everything is reachable"), "got {text}");
    std::fs::remove_dir_all(&dir).ok();
}
