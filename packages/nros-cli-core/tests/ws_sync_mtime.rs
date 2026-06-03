//! Phase 210.D.2 — per-pkg mtime guard regression suite for
//! `nros ws sync`.
//!
//! The original Phase 210.D acceptance bullet asks that editing any
//! `*.msg` file re-codegens ONLY the affected crate (plus its
//! downstream workspace deps). Before 210.D.2, `run_sync` re-emitted
//! every workspace + AMENT pkg on every run. These tests guard against
//! a regression to that behaviour:
//!
//!  * `sync_skips_unchanged_pkg` — second sync on an untouched
//!    workspace re-emits zero pkgs.
//!  * `sync_regenerates_only_affected_pkg` — touching one pkg's
//!    `*.msg` re-emits that pkg only.
//!  * `sync_regenerates_after_dep_msg_edit` — touching pkg-A's `.msg`
//!    when pkg-B depends on pkg-A re-emits BOTH (transitive freshness:
//!    downstream generated code references pkg-A's types).
//!  * `force_flag_bypasses_guard` — `--force` re-emits every pkg
//!    even when the mtime guard says they're fresh.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use nros_cli_core::cmd::ws::{Sub, SyncArgs, SyncReport, sync};

// --- fixture helpers ---------------------------------------------------------

fn unique_tempdir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir()
        .join(format!("nros_ws_sync_{name}_{}_{nanos}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Minimal colcon-style workspace fixture with one msg pkg.
///
/// Layout:
///   <root>/src/<pkg>/package.xml
///   <root>/src/<pkg>/msg/<Msg>.msg
fn write_msg_pkg(src_root: &Path, name: &str, msgs: &[(&str, &str)], deps: &[&str]) {
    let pkg_dir = src_root.join(name);
    fs::create_dir_all(pkg_dir.join("msg")).unwrap();
    let mut deps_xml = String::new();
    for d in deps {
        deps_xml.push_str(&format!("  <depend>{d}</depend>\n"));
    }
    let pxml = format!(
        "<?xml version=\"1.0\"?>\n\
         <package format=\"3\">\n  \
         <name>{name}</name>\n  \
         <version>0.1.0</version>\n  \
         <description>fixture</description>\n  \
         <maintainer email=\"x@x\">x</maintainer>\n  \
         <license>Apache-2.0</license>\n\
         {deps_xml}  \
         <member_of_group>rosidl_interface_packages</member_of_group>\n\
         </package>\n"
    );
    fs::write(pkg_dir.join("package.xml"), pxml).unwrap();
    for (msg_name, body) in msgs {
        fs::write(pkg_dir.join("msg").join(msg_name), body).unwrap();
    }
}

fn sync_args(ws_root: &Path, force: bool, verbose: bool) -> SyncArgs {
    SyncArgs {
        workspace: Some(ws_root.to_path_buf()),
        build_dir: PathBuf::from("generated"),
        ros_edition: "humble".to_string(),
        dry_run: false,
        check: false,
        verbose,
        force,
        nano_ros_path: None,
    }
}

fn do_sync(ws_root: &Path, force: bool) -> SyncReport {
    sync(sync_args(ws_root, force, false)).expect("ws sync")
}

/// Move a file's mtime BACKWARDS by `secs` seconds — simulates "the
/// generated tree was emitted comfortably before the source's
/// observed mtime, so the guard has a clean integer-second window".
/// Stable since Rust 1.75 via `File::set_modified`.
fn touch_back(p: &Path, secs: u64) {
    let now = SystemTime::now();
    let new = now - std::time::Duration::from_secs(secs);
    let f = fs::OpenOptions::new()
        .write(true)
        .open(p)
        .unwrap_or_else(|e| panic!("open {} for mtime adjust: {e}", p.display()));
    f.set_modified(new)
        .unwrap_or_else(|e| panic!("set_modified {}: {e}", p.display()));
}

/// Touch a file's mtime FORWARD by `secs` seconds — simulates an
/// edit without flaky `sleep` calls in tests.
fn touch_forward(p: &Path, secs: u64) {
    let now = SystemTime::now();
    let new = now + std::time::Duration::from_secs(secs);
    let f = fs::OpenOptions::new()
        .write(true)
        .open(p)
        .unwrap_or_else(|e| panic!("open {} for mtime adjust: {e}", p.display()));
    f.set_modified(new)
        .unwrap_or_else(|e| panic!("set_modified {}: {e}", p.display()));
}

// `Sub` is imported just so a missed dispatch break shows up at compile
// time — the tests drive `sync` directly so the unused-import lint is
// silenced via the underscore here.
#[allow(dead_code)]
fn _dispatch_compile_hint() -> Sub {
    Sub::Sync(SyncArgs {
        workspace: None,
        build_dir: PathBuf::new(),
        ros_edition: String::new(),
        dry_run: false,
        check: false,
        verbose: false,
        force: false,
        nano_ros_path: None,
    })
}

// --- tests -------------------------------------------------------------------

/// Helper: walk every interface input under a pkg's src dir BACKWARDS
/// in mtime so the generated tree (emitted by sync) is comfortably
/// newer. Without this, the freshly-written `.msg` files race the
/// freshly-written generated `Cargo.toml` in nanosecond-resolution
/// land and the guard's `>=` comparison can flip either way.
fn age_pkg_inputs(src_root: &Path, pkg: &str, secs_back: u64) {
    let pkg_dir = src_root.join(pkg);
    touch_back(&pkg_dir.join("package.xml"), secs_back);
    for subdir in &["msg", "srv", "action"] {
        let d = pkg_dir.join(subdir);
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                touch_back(&e.path(), secs_back);
            }
        }
    }
}

/// Second sync on an untouched workspace re-emits ZERO workspace pkgs.
/// AMENT-side pkgs may or may not skip depending on whether an AMENT
/// index is in the env, but workspace pkgs MUST be guard-skipped.
#[test]
fn sync_skips_unchanged_pkg() {
    let root = unique_tempdir("skips_unchanged");
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    write_msg_pkg(&src, "pkg_a", &[("Greeting.msg", "string text\n")], &[]);
    // Age inputs so the first-sync-emitted generated tree is newer.
    age_pkg_inputs(&src, "pkg_a", 10);

    // First sync emits pkg_a.
    let r1 = do_sync(&root, false);
    assert!(
        r1.regenerated.contains(&"pkg_a".to_string()),
        "first sync should regen pkg_a, got {r1:?}"
    );
    assert!(
        !r1.skipped_up_to_date.contains(&"pkg_a".to_string()),
        "first sync should NOT have skipped pkg_a, got {r1:?}"
    );

    // Second sync: guard fires (no source edit, generated > source).
    let r2 = do_sync(&root, false);
    assert!(
        r2.skipped_up_to_date.contains(&"pkg_a".to_string()),
        "second sync should have skipped pkg_a (mtime guard), got {r2:?}"
    );
    assert!(
        !r2.regenerated.contains(&"pkg_a".to_string()),
        "second sync should NOT have regenerated pkg_a, got {r2:?}"
    );
}

/// Touching one pkg's `.msg` re-emits ONLY that pkg. Sibling pkgs
/// with no dep relationship are left in the skipped list.
#[test]
fn sync_regenerates_only_affected_pkg() {
    let root = unique_tempdir("only_affected");
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    write_msg_pkg(&src, "pkg_a", &[("Hello.msg", "string text\n")], &[]);
    write_msg_pkg(&src, "pkg_b", &[("World.msg", "int32 value\n")], &[]);
    age_pkg_inputs(&src, "pkg_a", 10);
    age_pkg_inputs(&src, "pkg_b", 10);

    do_sync(&root, false);

    // Edit pkg_a's .msg (mtime jumps forward — defeats the guard).
    touch_forward(&src.join("pkg_a").join("msg").join("Hello.msg"), 5);

    let r = do_sync(&root, false);
    assert!(
        r.regenerated.contains(&"pkg_a".to_string()),
        "pkg_a should regen after its .msg edit, got {r:?}"
    );
    assert!(
        r.skipped_up_to_date.contains(&"pkg_b".to_string()),
        "pkg_b should be guard-skipped (unrelated to pkg_a), got {r:?}"
    );
    assert!(
        !r.regenerated.contains(&"pkg_b".to_string()),
        "pkg_b should NOT regen, got {r:?}"
    );
}

/// pkg_b depends on pkg_a. Editing pkg_a's `.msg` must regen BOTH —
/// pkg_b's generated `use pkg_a::…` sites might reference different
/// symbols after pkg_a's CDR layout changes. This is the subtle case
/// — without the transitive-freshness rule, pkg_b would be skipped
/// because its own inputs didn't move.
#[test]
fn sync_regenerates_after_dep_msg_edit() {
    let root = unique_tempdir("dep_edit");
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    write_msg_pkg(&src, "pkg_a", &[("Base.msg", "string text\n")], &[]);
    write_msg_pkg(
        &src,
        "pkg_b",
        // pkg_b's .msg references pkg_a/Base — codegen output thus
        // depends on pkg_a's generated crate.
        &[("Wrap.msg", "pkg_a/Base inner\nint32 extra\n")],
        &["pkg_a"],
    );
    age_pkg_inputs(&src, "pkg_a", 10);
    age_pkg_inputs(&src, "pkg_b", 10);

    do_sync(&root, false);

    // Edit pkg_a's `.msg` only.
    touch_forward(&src.join("pkg_a").join("msg").join("Base.msg"), 5);

    let r = do_sync(&root, false);
    assert!(
        r.regenerated.contains(&"pkg_a".to_string()),
        "pkg_a should regen after .msg edit, got {r:?}"
    );
    assert!(
        r.regenerated.contains(&"pkg_b".to_string()),
        "pkg_b should regen transitively (it depends on pkg_a), got {r:?}. \
         Without the transitive-freshness rule, the mtime guard would \
         silently leak stale generated code into pkg_b."
    );
    assert!(
        !r.skipped_up_to_date.contains(&"pkg_b".to_string()),
        "pkg_b should NOT appear in skipped list, got {r:?}"
    );
}

/// `--force` re-emits every pkg unconditionally — the mtime guard
/// (own-input AND dep-regen checks) is bypassed.
#[test]
fn force_flag_bypasses_guard() {
    let root = unique_tempdir("force_flag");
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    write_msg_pkg(&src, "pkg_a", &[("Greeting.msg", "string text\n")], &[]);
    age_pkg_inputs(&src, "pkg_a", 10);

    do_sync(&root, false);

    // Forced sync: pkg_a regens despite no .msg changes.
    let r = do_sync(&root, true);
    assert!(
        r.regenerated.contains(&"pkg_a".to_string()),
        "--force should bypass guard and regen pkg_a, got {r:?}"
    );
    assert!(
        !r.skipped_up_to_date.contains(&"pkg_a".to_string()),
        "--force should NOT mark pkg_a as skipped, got {r:?}"
    );
}
