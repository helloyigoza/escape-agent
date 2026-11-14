use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::Path;

use super::index::{path_matches_scope, relative_scope, tree_root_for, with_index};
use crate::{ToolContext, ToolError};

/// Native `affected` operation: discover test files impacted by files or symbols.
pub(in crate::code_map) fn native_affected(
    ctx: &ToolContext,
    target_path: &Path,
    seed_csv: &str,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let raw_seeds: Vec<String> = seed_csv
            .split([',', '\n'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let mut seed_ids: HashSet<usize> = HashSet::new();
        for seed in &raw_seeds {
            let seed_norm = seed.replace('\\', "/").to_lowercase();
            let mut matched_file = false;
            for sym in ws.symbols_in_scope(&root, target_path) {
                if sym.rel_path.to_lowercase().ends_with(&seed_norm)
                    || sym.rel_path.to_lowercase().contains(&seed_norm)
                {
                    seed_ids.insert(sym.id);
                    matched_file = true;
                }
            }
            if !matched_file {
                for id in ws.find_matching_symbols(scope.as_deref(), seed) {
                    seed_ids.insert(id);
                }
            }
        }

        // Reverse BFS to find callers and test files/functions.
        let mut visited = seed_ids.clone();
        let mut queue: VecDeque<usize> = seed_ids.into_iter().collect();
        while let Some(node) = queue.pop_front() {
            for &caller in &ws.callers[node] {
                if visited.insert(caller) {
                    queue.push_back(caller);
                }
            }
        }

        let mut affected_tests: Vec<String> = Vec::new();
        let mut seen_tests = HashSet::new();
        for &id in &visited {
            let s = &ws.all_symbols[id];
            if is_test_path_or_symbol(&s.rel_path, &s.sym.name)
                && seen_tests.insert(format!("{}:{}", s.rel_path, s.sym.name))
            {
                affected_tests.push(format!(
                    "  test {} ({}:{})",
                    s.sym.name, s.rel_path, s.start_line
                ));
                if affected_tests.len() >= limit {
                    break;
                }
            }
        }

        // Also match test files whose text contains seeded symbol names when edge resolution misses.
        if affected_tests.len() < limit && !raw_seeds.is_empty() {
            for path in &ws.file_order {
                let Some(file) = ws.files_by_path.get(path) else {
                    continue;
                };
                if !path_matches_scope(&file.rel_path, scope.as_deref()) {
                    continue;
                }
                if !is_test_path_or_symbol(&file.rel_path, "") {
                    continue;
                }
                let joined = file.lines.join("\n");
                let hits = raw_seeds.iter().any(|seed| {
                    let stem = Path::new(seed)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(seed);
                    !stem.is_empty() && joined.contains(stem)
                });
                if hits && seen_tests.insert(file.rel_path.clone()) {
                    affected_tests.push(format!("  test_file {}", file.rel_path));
                    if affected_tests.len() >= limit {
                        break;
                    }
                }
            }
        }

        let count = affected_tests.len();
        let body = if affected_tests.is_empty() {
            format!("<affected seeds=\"{seed_csv}\" tests=\"0\"/>")
        } else {
            format!(
                "<affected seeds=\"{seed_csv}\" tests=\"{count}\">\n{}\n</affected>",
                affected_tests.join("\n")
            )
        };
        Ok((body, count))
    })
}

pub(super) fn is_test_path_or_symbol(rel_path: &str, name: &str) -> bool {
    let p = rel_path.to_lowercase();
    let n = name.to_lowercase();
    p.contains("/tests/")
        || p.starts_with("tests/")
        || p.contains("_test.")
        || p.contains(".test.")
        || p.contains(".spec.")
        || n.starts_with("test_")
}

/// Native `trace` operation: parse `path:line` frames from a stack trace and
/// map each to its enclosing symbol + body within `max_tokens`.
pub(in crate::code_map) fn native_trace(
    ctx: &ToolContext,
    target_path: &Path,
    trace_text: &str,
    max_tokens: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let frames = extract_trace_frames(trace_text);
        if frames.is_empty() {
            return Ok((
                "No `file:line` stack frames detected in trace input.".to_string(),
                0,
            ));
        }

        let max_chars = max_tokens.saturating_mul(4);
        let mut out = String::from("<trace>\n");
        let mut resolved = 0usize;

        for (file_ref, line_num) in frames {
            let ids = ws.find_by_file_line(&file_ref, line_num);
            if let Some(&sym_id) = ids.first() {
                let sym = &ws.all_symbols[sym_id];
                let file_path = root.join(&sym.rel_path);
                let snippet = ws
                    .files_by_path
                    .get(&file_path)
                    .map_or_else(String::new, |f| {
                        let start = sym.start_line.saturating_sub(1);
                        let end = sym.end_line.min(f.lines.len()).min(start + 24);
                        f.lines[start..end].join("\n")
                    });
                let entry = format!(
                    "  <frame file=\"{}:{line_num}\" symbol=\"{}\" span=\"{}..{}\">\n{}\n  </frame>\n",
                    sym.rel_path, sym.sym.name, sym.start_line, sym.end_line, snippet
                );
                if out.len() + entry.len() <= max_chars || resolved == 0 {
                    out.push_str(&entry);
                    resolved += 1;
                }
            }
        }
        out.push_str("</trace>");
        Ok((out, resolved))
    })
}

fn extract_trace_frames(trace_text: &str) -> Vec<(String, usize)> {
    let mut frames = Vec::new();
    let mut seen = HashSet::new();
    for raw in trace_text.split_whitespace() {
        let trimmed =
            raw.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | ',' | ';' | '"' | '\''));
        let parts: Vec<&str> = trimmed.split(':').collect();
        if parts.len() >= 2 {
            // Match either `path:line` or `path:line:col`.
            for win in parts.windows(2) {
                if let Ok(line_num) = win[1].parse::<usize>()
                    && line_num > 0
                    && (win[0].contains('.') || win[0].contains('/') || win[0].contains('\\'))
                    && seen.insert((win[0].to_string(), line_num))
                {
                    frames.push((win[0].to_string(), line_num));
                    break;
                }
            }
        }
    }
    frames
}

/// Fast pure-Rust impacted test discovery for a list of changed workspace paths.
pub(crate) fn native_affected_tests_for_files(
    ctx: &ToolContext,
    root: &Path,
    changed_files: &[String],
) -> Vec<String> {
    with_index(ctx, root, |ws| {
        let mut seed_ids = Vec::new();
        for changed in changed_files {
            let norm = changed.replace('\\', "/");
            let norm = norm.strip_prefix("./").unwrap_or(&norm);
            if norm.is_empty() {
                continue;
            }
            for sym in &ws.all_symbols {
                let matched = sym.rel_path == norm
                    || sym.rel_path.ends_with(&format!("/{norm}"))
                    || norm.ends_with(&format!("/{}", sym.rel_path));
                if matched {
                    seed_ids.push(sym.id);
                }
            }
        }
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        for id in seed_ids {
            if visited.insert(id) {
                queue.push_back((id, 0usize));
            }
        }
        let mut test_files = BTreeSet::new();
        while let Some((cur, depth)) = queue.pop_front() {
            let s = &ws.all_symbols[cur];
            if is_test_path_or_symbol(&s.rel_path, &s.sym.name) {
                test_files.insert(s.rel_path.clone());
            }
            if depth >= 4 {
                continue;
            }
            for &caller_id in &ws.callers[cur] {
                if visited.insert(caller_id) {
                    queue.push_back((caller_id, depth + 1));
                }
            }
        }
        Ok(test_files.into_iter().take(12).collect())
    })
    .unwrap_or_default()
}
