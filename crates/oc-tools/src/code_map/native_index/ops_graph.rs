use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::{Command, Stdio};

use super::super::files::relative_to;
use super::index::{IndexedSymbol, relative_scope, tree_root_for, with_index};
use crate::{ToolContext, ToolError};
use std::fmt::Write as _;
use std::io::Write as _;

/// Native `impact` operation: BFS reverse-caller blast radius.
pub(in crate::code_map) fn native_impact(
    ctx: &ToolContext,
    target_path: &Path,
    query: &str,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let seeds = ws.find_matching_symbols(scope.as_deref(), query);
        if seeds.is_empty() {
            return Ok((format!("No symbol found for impact query '{query}'."), 0));
        }

        let mut visited: HashSet<usize> = seeds.iter().copied().collect();
        let mut queue: VecDeque<(usize, usize)> = seeds.iter().map(|&id| (id, 0)).collect();
        let mut impacted: Vec<(usize, &IndexedSymbol)> = Vec::new();

        while let Some((node, depth)) = queue.pop_front() {
            for &caller in &ws.callers[node] {
                if visited.insert(caller) {
                    let next_depth = depth + 1;
                    impacted.push((next_depth, &ws.all_symbols[caller]));
                    if impacted.len() >= limit {
                        break;
                    }
                    queue.push_back((caller, next_depth));
                }
            }
            if impacted.len() >= limit {
                break;
            }
        }

        let seed_desc: Vec<String> = seeds
            .iter()
            .take(3)
            .map(|&id| {
                let s = &ws.all_symbols[id];
                format!("{} ({}:{})", s.sym.name, s.rel_path, s.start_line)
            })
            .collect();
        let mut out = format!(
            "<impact target=\"{query}\" seeds=\"{}\" dependents=\"{}\">\n",
            seed_desc.join(", "),
            impacted.len()
        );
        for (depth, sym) in &impacted {
            let _ = writeln!(
                out,
                "  d={depth} [{}] {} ({}:{})",
                sym.sym.kind.as_str(),
                sym.sym.name,
                sym.rel_path,
                sym.start_line
            );
        }
        out.push_str("</impact>");
        Ok((out, impacted.len()))
    })
}

/// Native `connect` operation: shortest undirected graph path between `a` and `b`.
pub(in crate::code_map) fn native_connect(
    ctx: &ToolContext,
    target_path: &Path,
    from_query: &str,
    to_query: &str,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let starts = ws.find_matching_symbols(scope.as_deref(), from_query);
        let targets: HashSet<usize> = ws
            .find_matching_symbols(scope.as_deref(), to_query)
            .into_iter()
            .collect();

        if starts.is_empty() || targets.is_empty() {
            return Ok((
                format!("No path: could not resolve symbols for '{from_query}' and '{to_query}'."),
                0,
            ));
        }

        let mut parent: HashMap<usize, usize> = HashMap::new();
        let mut visited: HashSet<usize> = starts.iter().copied().collect();
        let mut queue: VecDeque<usize> = starts.iter().copied().collect();
        let mut reached = None;

        while let Some(curr) = queue.pop_front() {
            if targets.contains(&curr) && !starts.contains(&curr) {
                reached = Some(curr);
                break;
            }
            for &next in ws.callees[curr].iter().chain(ws.callers[curr].iter()) {
                if visited.insert(next) {
                    parent.insert(next, curr);
                    if targets.contains(&next) {
                        reached = Some(next);
                        break;
                    }
                    queue.push_back(next);
                }
            }
            if reached.is_some() {
                break;
            }
        }

        let Some(end) = reached else {
            return Ok((
                format!("No call-graph path found between '{from_query}' and '{to_query}'."),
                0,
            ));
        };

        let mut path = vec![end];
        let mut cur = end;
        while let Some(&prev) = parent.get(&cur) {
            path.push(prev);
            cur = prev;
        }
        path.reverse();

        let hops = path.len().saturating_sub(1);
        let mut out =
            format!("<connect from=\"{from_query}\" to=\"{to_query}\" hops=\"{hops}\">\n");
        for (idx, &node_id) in path.iter().enumerate() {
            let s = &ws.all_symbols[node_id];
            let _ = writeln!(
                out,
                "  {}. {} [{}] ({}:{})",
                idx + 1,
                s.sym.name,
                s.sym.kind.as_str(),
                s.rel_path,
                s.start_line
            );
        }
        out.push_str("</connect>");
        Ok((out, hops))
    })
}

/// Native `cochange`: compute git historical co-edit partners for a file.
pub(in crate::code_map) fn native_cochange(
    ctx: &ToolContext,
    target_path: &Path,
    limit: usize,
) -> (String, usize) {
    let root = tree_root_for(ctx, target_path);
    let target_rel = relative_to(&root, target_path);
    let hashes_out = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["log", "-n", "60", "--format=%H", "--", &target_rel])
        .output();
    let Ok(hashes_res) = hashes_out else {
        return (format!("No git history available for `{target_rel}`."), 0);
    };
    if !hashes_res.status.success() {
        return (format!("No git co-change history for `{target_rel}`."), 0);
    }
    let hashes: Vec<String> = String::from_utf8_lossy(&hashes_res.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    if hashes.is_empty() {
        return (format!("0 commits found touching `{target_rel}`."), 0);
    }

    let mut partner_counts: HashMap<String, usize> = HashMap::new();
    if let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args([
            "diff-tree",
            "--stdin",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--root",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let mut payload = hashes.join("\n");
            payload.push('\n');
            let _ = stdin.write_all(payload.as_bytes());
        }
        if let Ok(files_res) = child.wait_with_output()
            && files_res.status.success()
        {
            for f in String::from_utf8_lossy(&files_res.stdout).lines() {
                let f = f.trim();
                if !f.is_empty() && f != target_rel {
                    *partner_counts.entry(f.to_string()).or_default() += 1;
                }
            }
        }
    }

    let mut ranked: Vec<(String, usize)> = partner_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total = ranked.len();
    let mut text = format!(
        "Co-change partners for `{target_rel}` (over {} commits, {} partners):\n",
        hashes.len(),
        total
    );
    for (file, cnt) in ranked.into_iter().take(limit) {
        let _ = writeln!(text, "  {cnt:>3}x  {file}");
    }
    (text, total)
}
