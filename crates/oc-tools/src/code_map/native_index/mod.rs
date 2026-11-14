//! Cached pure-Rust workspace symbol and call-graph index.
//!
//! Powers every `code_map` operation (`pack_task`, `map`, `communities`,
//! `outline`, `symbol`, `expand`, `impact`, `references`, `connect`,
//! `affected`, `trace`, `batch`, `lego`, `slice`, `cochange`, `doc_drift`,
//! `flags`, `quality`) and `symbol_edit` native fallback when `ripwire` is
//! absent or falls back, validating file `(len, modified)` stamps incrementally
//! so unchanged files are never re-parsed.

mod index;
mod ops_affected;
mod ops_deep;
mod ops_graph;
mod ops_map;
mod ops_symbol;
mod pack;
mod parse;
mod quality;

pub(in crate::code_map) use self::index::relative_scope;
pub(crate) use self::index::tree_root_for;
pub(crate) use self::ops_affected::native_affected_tests_for_files;
pub(in crate::code_map) use self::ops_affected::{native_affected, native_trace};
pub(in crate::code_map) use self::ops_deep::{
    native_doc_drift, native_flags, native_lego, native_slice,
};
pub(in crate::code_map) use self::ops_graph::{native_cochange, native_connect, native_impact};
pub(in crate::code_map) use self::ops_map::{native_communities, native_map, native_outline};
pub(crate) use self::ops_symbol::{NativeSymbolLocation, find_native_symbol_locations};
pub(in crate::code_map) use self::ops_symbol::{
    locate_symbol_line_col, native_expand, native_symbol,
};
pub(in crate::code_map) use self::pack::native_pack_task;
pub(crate) use self::pack::{NativePartitionSlice, native_partition_task};
pub(in crate::code_map) use self::quality::native_quality;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use crate::ToolContext;
    use tempfile::TempDir;

    fn test_ctx(dir: &Path) -> ToolContext {
        ToolContext {
            session_id: "test".to_string(),
            directory: dir.to_path_buf(),
            cwd: dir.to_path_buf(),
            cancel: None,
        }
    }

    #[test]
    fn native_index_covers_expand_impact_connect_communities_affected_trace() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        let tests_dir = dir.path().join("tests");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&tests_dir).unwrap();

        fs::write(
            src.join("core.rs"),
            "pub fn leaf_helper(x: i32) -> i32 {\n    x + 1\n}\n",
        )
        .unwrap();
        fs::write(
            src.join("service.rs"),
            "pub fn process_req(v: i32) -> i32 {\n    // comment\n    leaf_helper(v)\n}\n\npub fn entrypoint() -> i32 {\n    process_req(41)\n}\n",
        )
        .unwrap();
        fs::write(
            tests_dir.join("service_test.rs"),
            "#[test]\nfn test_entrypoint() {\n    let _ = entrypoint();\n}\n",
        )
        .unwrap();

        let ctx = test_ctx(dir.path());

        let (sym_out, sym_cnt) = native_symbol(&ctx, dir.path(), "process_req", 10).unwrap();
        assert_eq!(sym_cnt, 1);
        assert!(sym_out.contains("callers:[entrypoint"));
        assert!(sym_out.contains("calls:[leaf_helper"));

        let (exp_out, _) =
            native_expand(&ctx, dir.path(), "process_req", None, None, true).unwrap();
        assert!(exp_out.contains("leaf_helper(v)"));
        assert!(!exp_out.contains("// comment"));

        let (imp_out, imp_cnt) = native_impact(&ctx, dir.path(), "leaf_helper", 10).unwrap();
        assert!(imp_cnt >= 2);
        assert!(imp_out.contains("process_req"));
        assert!(imp_out.contains("entrypoint"));

        let (conn_out, hops) =
            native_connect(&ctx, dir.path(), "entrypoint", "leaf_helper").unwrap();
        assert!(hops >= 2);
        assert!(conn_out.contains("process_req"));

        let (comm_out, mods) = native_communities(&ctx, dir.path(), None, 10).unwrap();
        assert!(mods >= 2);
        assert!(comm_out.contains("src"));
        assert!(comm_out.contains("bridge"));

        let (aff_out, aff_cnt) = native_affected(&ctx, dir.path(), "leaf_helper", 10).unwrap();
        assert!(aff_cnt >= 1);
        assert!(aff_out.contains("test_entrypoint"));

        let (tr_out, tr_cnt) =
            native_trace(&ctx, dir.path(), "panicked at src/service.rs:3:5", 1200).unwrap();
        assert_eq!(tr_cnt, 1);
        assert!(tr_out.contains("process_req"));

        let (pack_out, pack_cnt) =
            native_pack_task(&ctx, dir.path(), "process_req helper", 1200, 10).unwrap();
        assert!(pack_cnt >= 1);
        assert!(pack_out.contains("<sigs>"));
        assert!(pack_out.contains("<bodies>"));
    }

    #[test]
    fn native_expand_respects_char_brace_literals_and_affected_avoids_suffix_collisions() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        let tests_dir = dir.path().join("tests");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&tests_dir).unwrap();

        fs::write(
            src.join("lib.rs"),
            "pub fn has_brace<'a>(_s: &'a str, c: char) -> bool {\n    let close = '}';\n    c == close\n}\n",
        )
        .unwrap();
        fs::write(
            src.join("other_lib.rs"),
            "pub fn unrelated_fn() -> i32 { 1 }\n",
        )
        .unwrap();
        fs::write(
            tests_dir.join("other_test.rs"),
            "#[test]\nfn test_unrelated() { let _ = unrelated_fn(); }\n",
        )
        .unwrap();

        let ctx = test_ctx(dir.path());
        let (exp_out, _) = native_expand(&ctx, dir.path(), "has_brace", None, None, false).unwrap();
        assert!(
            exp_out.contains("c == close"),
            "expand must not terminate early at char literal '}}': {exp_out}"
        );

        let affected = native_affected_tests_for_files(&ctx, dir.path(), &["lib.rs".to_string()]);
        assert!(
            affected.is_empty(),
            "lib.rs must not false-match tests referencing other_lib.rs: {affected:?}"
        );
    }

    #[test]
    fn native_cochange_ranks_git_partners_in_batch() {
        let dir = TempDir::new().unwrap();
        let run_git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .expect("git installed");
            assert!(status.success(), "git {args:?} failed");
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);

        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/a.rs"), "pub fn a() {}\n").unwrap();
        fs::write(dir.path().join("src/b.rs"), "pub fn b() {}\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-q", "-m", "init"]);

        fs::write(dir.path().join("src/a.rs"), "pub fn a() { /* v2 */ }\n").unwrap();
        fs::write(dir.path().join("src/b.rs"), "pub fn b() { /* v2 */ }\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-q", "-m", "coedit"]);

        let ctx = test_ctx(dir.path());
        let (report, count) = native_cochange(&ctx, &dir.path().join("src/a.rs"), 10);
        assert_eq!(count, 1, "unexpected report: {report}");
        assert!(
            report.contains("2x  src/b.rs"),
            "unexpected report: {report}"
        );
    }
}
