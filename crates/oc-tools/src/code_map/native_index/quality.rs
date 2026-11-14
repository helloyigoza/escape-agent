use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use super::super::{SymbolKind, Visibility};
use super::index::{
    CachedWorkspace, IndexedSymbol, path_matches_scope, relative_scope, tree_root_for, with_index,
};
use super::ops_affected::is_test_path_or_symbol;
use crate::{ToolContext, ToolError};

/// Native structural quality lens (`delta`, `dead_code`, `clones`, `hotspots`, `deps`, `seams`).
pub(in crate::code_map) fn native_quality(
    ctx: &ToolContext,
    target_path: &Path,
    mode: &str,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let rows = match mode {
            "dead_code" => quality_dead_code(ws, scope.as_deref()),
            "hotspots" => quality_hotspots(ws, scope.as_deref(), limit),
            "clones" => quality_clones(ws, &root, scope.as_deref()),
            _ => quality_edges(ws, scope.as_deref(), limit),
        };
        let total = rows.len();
        let mut out = format!("Quality `{mode}` ({total} findings):\n");
        for row in rows.into_iter().take(limit) {
            out.push_str(&row);
            out.push('\n');
        }
        Ok((out, total))
    })
}

fn quality_dead_code(ws: &CachedWorkspace, scope: Option<&str>) -> Vec<String> {
    let mut rows = Vec::new();
    for sym in &ws.all_symbols {
        if !path_matches_scope(&sym.rel_path, scope) {
            continue;
        }
        if matches!(sym.sym.kind, SymbolKind::Function | SymbolKind::Method)
            && sym.sym.visibility == Visibility::Private
            && ws.fan_in(sym.id) == 0
            && sym.sym.name != "main"
            && !is_test_path_or_symbol(&sym.rel_path, &sym.sym.name)
        {
            rows.push(format!(
                "  {}:{} [private dead?] {}",
                sym.rel_path, sym.start_line, sym.sym.signature
            ));
        }
    }
    rows
}

fn quality_hotspots(ws: &CachedWorkspace, scope: Option<&str>, limit: usize) -> Vec<String> {
    let mut scored: Vec<(usize, &IndexedSymbol)> = ws
        .all_symbols
        .iter()
        .filter(|s| path_matches_scope(&s.rel_path, scope))
        .map(|s| {
            let span = s.end_line.saturating_sub(s.start_line) + 1;
            let deg = ws.fan_in(s.id) + ws.fan_out(s.id) + 1;
            (span * deg, s)
        })
        .collect();
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    scored
        .into_iter()
        .take(limit)
        .map(|(score, s)| {
            format!(
                "  score={score:<4} {}:{} [{}] {}",
                s.rel_path,
                s.start_line,
                s.sym.kind.as_str(),
                s.sym.name
            )
        })
        .collect()
}

fn quality_clones(ws: &CachedWorkspace, root: &Path, scope: Option<&str>) -> Vec<String> {
    let mut by_norm: HashMap<String, Vec<String>> = HashMap::new();
    for sym in &ws.all_symbols {
        if !path_matches_scope(&sym.rel_path, scope)
            || sym.end_line.saturating_sub(sym.start_line) < 4
        {
            continue;
        }
        let abs = root.join(&sym.rel_path);
        let Some(file) = ws.files_by_path.get(&abs) else {
            continue;
        };
        let start = sym.start_line;
        let end = sym.end_line.min(file.lines.len());
        if start >= end {
            continue;
        }
        let body: String = file.lines[start..end]
            .iter()
            .map(|l| l.trim())
            .collect::<Vec<_>>()
            .join("\n");
        if body.len() >= 40 {
            by_norm.entry(body).or_default().push(format!(
                "{}:{} {}",
                sym.rel_path, sym.start_line, sym.sym.name
            ));
        }
    }
    by_norm
        .into_values()
        .filter(|locs| locs.len() >= 2)
        .map(|locs| format!("  duplicate body: {}", locs.join(" == ")))
        .collect()
}

fn quality_edges(ws: &CachedWorkspace, scope: Option<&str>, limit: usize) -> Vec<String> {
    let mut edges: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (caller_id, callees) in ws.callees.iter().enumerate() {
        let from_mod = ws.all_symbols[caller_id].rel_path.clone();
        if !path_matches_scope(&from_mod, scope) {
            continue;
        }
        for &callee_id in callees {
            let to_mod = &ws.all_symbols[callee_id].rel_path;
            if &from_mod != to_mod {
                *edges.entry((from_mod.clone(), to_mod.clone())).or_default() += 1;
            }
        }
    }
    let mut sorted: Vec<((String, String), usize)> = edges.into_iter().collect();
    sorted.sort_by_key(|a| std::cmp::Reverse(a.1));
    sorted
        .into_iter()
        .take(limit)
        .map(|((a, b), count)| format!("  {a} -> {b} ({count} refs)"))
        .collect()
}
