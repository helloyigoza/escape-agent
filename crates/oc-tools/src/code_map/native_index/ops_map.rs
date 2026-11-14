use std::collections::HashMap;
use std::path::Path;

use super::super::files::relative_to;
use super::super::{SymbolKind, Visibility};
use super::index::{
    CachedWorkspace, IndexedSymbol, path_matches_scope, relative_scope, tree_root_for,
    usize_to_f32, with_index,
};
use crate::{ToolContext, ToolError};
use std::fmt::Write as _;

/// Cached pure-Rust `outline` for a directory tree using mtime-validated symbols.
pub(in crate::code_map) fn native_outline(
    ctx: &ToolContext,
    target_path: &Path,
    limit: usize,
) -> Result<(String, usize, bool), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let mut scoped_files = 0usize;
        let mut total_symbols = 0usize;
        let mut shown_symbols = 0usize;
        let mut body = String::new();

        for path in &ws.file_order {
            let Some(file) = ws.files_by_path.get(path) else {
                continue;
            };
            if !path_matches_scope(&file.rel_path, scope.as_deref()) {
                continue;
            }
            scoped_files += 1;
            if file.symbols.is_empty() {
                continue;
            }
            total_symbols += file.symbols.len();
            if shown_symbols >= limit {
                continue;
            }
            let rel = relative_to(&ctx.directory, &file.path);
            let _ = writeln!(body, "\n{rel} ({} symbols):", file.symbols.len());
            for sym in &file.symbols {
                if shown_symbols >= limit {
                    break;
                }
                let _ = writeln!(
                    body,
                    "  L{:<4} [{}] {}",
                    sym.start_line,
                    sym.sym.kind.as_str(),
                    sym.sym.signature
                );
                shown_symbols += 1;
            }
        }

        let mut out = format!(
            "Directory outline for {} ({scoped_files} files):\n{body}",
            target_path.display()
        );
        if shown_symbols < total_symbols {
            let _ = writeln!(
                out,
                "\n[showing {shown_symbols} of {total_symbols} symbols; pass a specific file `path` to narrow down]"
            );
        }
        Ok((out, total_symbols, shown_symbols < total_symbols))
    })
}

type ScoredFileBucket<'a> = (f32, &'a str, Vec<(f32, &'a IndexedSymbol)>);

fn score_map_symbol(
    ws: &CachedWorkspace,
    sym: &IndexedSymbol,
    q_terms: &[String],
    rank_by: Option<&str>,
) -> f32 {
    let name_low = sym.sym.name.to_lowercase();
    let sig_low = sym.sym.signature.to_lowercase();
    let doc_low = sym.sym.doc.as_deref().unwrap_or("").to_lowercase();

    let mut weight = if q_terms.is_empty() {
        match sym.sym.kind {
            SymbolKind::Struct | SymbolKind::Class | SymbolKind::Interface | SymbolKind::Trait => {
                3.0
            }
            SymbolKind::Function | SymbolKind::Method => 2.0,
            _ => 1.0,
        }
    } else {
        let mut total = 0.0_f32;
        for term in q_terms {
            if name_low == *term {
                total += 15.0;
            } else if name_low.contains(term) {
                total += 8.0;
            }
            if sig_low.contains(term) {
                total += 3.0;
            }
            if doc_low.contains(term) {
                total += 2.0;
            }
        }
        total
    };

    if weight == 0.0 {
        return 0.0;
    }

    let fan_in = usize_to_f32(ws.fan_in(sym.id));
    let fan_out = usize_to_f32(ws.fan_out(sym.id));
    let graph_boost = match rank_by {
        Some("coupling") => fan_out * 1.5 + fan_in * 0.5,
        Some("risk") => (fan_in + 1.0) * (fan_out + 1.0) * 0.4,
        _ => fan_in * 1.4 + fan_out * 0.3,
    };
    weight += graph_boost;

    if sym.sym.visibility == Visibility::Public {
        weight *= 1.2;
    }
    weight
}

/// Native `map` operation boosted with call-graph centrality and ranking modes.
pub(in crate::code_map) fn native_map(
    ctx: &ToolContext,
    target_path: &Path,
    query: &str,
    rank_by: Option<&str>,
    max_tokens: usize,
    limit: usize,
) -> Result<(String, usize, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let q_terms: Vec<String> = query
            .split_whitespace()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();

        let mut per_file: HashMap<usize, ScoredFileBucket<'_>> = HashMap::new();
        for sym in &ws.all_symbols {
            if !path_matches_scope(&sym.rel_path, scope.as_deref()) {
                continue;
            }
            let rel_lower = sym.rel_path.to_lowercase();
            let path_bonus: f32 = q_terms
                .iter()
                .filter(|term| rel_lower.contains(term.as_str()))
                .map(|_| 4.0)
                .sum();
            let weight = score_map_symbol(ws, sym, &q_terms, rank_by);
            if weight == 0.0 && path_bonus == 0.0 {
                continue;
            }

            let entry = per_file
                .entry(sym.file_idx)
                .or_insert_with(|| (path_bonus, sym.rel_path.as_str(), Vec::new()));
            entry.2.push((weight, sym));
        }

        let mut ranked_files: Vec<ScoredFileBucket<'_>> = per_file.into_values().collect();
        for (total, _, syms) in &mut ranked_files {
            syms.sort_by(|a, b| b.0.total_cmp(&a.0));
            let top_sum: f32 = syms.iter().take(5).map(|(s, _)| *s).sum();
            *total += top_sum;
        }
        ranked_files.sort_by(|a, b| b.0.total_cmp(&a.0));

        let mut out = format!(
            "<code_map query=\"{query}\" root=\"{}\">\n",
            relative_to(&ctx.directory, target_path)
        );
        let max_chars = max_tokens.saturating_mul(4);
        let mut total_files = 0usize;
        let mut total_symbols = 0usize;

        for (_, rel, syms) in ranked_files.into_iter().take(limit) {
            if out.len() >= max_chars {
                break;
            }
            let _ = writeln!(out, "{rel}:");
            total_files += 1;
            for (_, sym) in syms.into_iter().take(8) {
                if out.len() >= max_chars {
                    break;
                }
                let fan_in = ws.fan_in(sym.id);
                if fan_in > 0 {
                    let _ = writeln!(
                        out,
                        "  L{:<4} {} (callers:{fan_in})",
                        sym.start_line, sym.sym.signature
                    );
                } else {
                    let _ = writeln!(out, "  L{:<4} {}", sym.start_line, sym.sym.signature);
                }
                total_symbols += 1;
            }
        }
        out.push_str("</code_map>\n");
        Ok((out, total_files, total_symbols))
    })
}

/// Native `communities` operation: module directory clusters, hubs, and cross-module bridges.
pub(in crate::code_map) fn native_communities(
    ctx: &ToolContext,
    target_path: &Path,
    filter: Option<&str>,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let mut clusters: HashMap<String, Vec<&IndexedSymbol>> = HashMap::new();
        let mut sym_cluster: HashMap<usize, String> = HashMap::new();

        for sym in &ws.all_symbols {
            if !path_matches_scope(&sym.rel_path, scope.as_deref()) {
                continue;
            }
            let dir = cluster_name(&sym.rel_path);
            sym_cluster.insert(sym.id, dir.clone());
            clusters.entry(dir).or_default().push(sym);
        }

        let mut bridges: HashMap<(String, String), usize> = HashMap::new();
        for (caller_id, callees) in ws.callees.iter().enumerate() {
            let Some(from_c) = sym_cluster.get(&caller_id) else {
                continue;
            };
            for callee_id in callees {
                let Some(to_c) = sym_cluster.get(callee_id) else {
                    continue;
                };
                if from_c != to_c {
                    let pair = if from_c <= to_c {
                        (from_c.clone(), to_c.clone())
                    } else {
                        (to_c.clone(), from_c.clone())
                    };
                    *bridges.entry(pair).or_default() += 1;
                }
            }
        }

        let filter_low = filter
            .map(|f| f.trim().to_lowercase())
            .filter(|f| !f.is_empty());
        let mut sorted: Vec<(String, Vec<&IndexedSymbol>)> = clusters
            .into_iter()
            .filter(|(name, _)| {
                filter_low
                    .as_ref()
                    .is_none_or(|f| name.to_lowercase().contains(f))
            })
            .collect();
        sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

        let mut out = format!(
            "<communities modules=\"{}\" symbols=\"{}\">\n",
            sorted.len(),
            ws.all_symbols.len()
        );
        let mut emitted = 0usize;
        for (name, mut syms) in sorted.into_iter().take(limit) {
            syms.sort_by(|a, b| {
                (ws.fan_in(b.id) + ws.fan_out(b.id)).cmp(&(ws.fan_in(a.id) + ws.fan_out(a.id)))
            });
            let hubs: Vec<String> = syms
                .iter()
                .take(4)
                .map(|s| format!("{}({}:{})", s.sym.name, s.rel_path, s.start_line))
                .collect();
            let _ = writeln!(
                out,
                "  <community id=\"{name}\" size=\"{}\" hubs=\"{}\"/>",
                syms.len(),
                hubs.join(", ")
            );
            emitted += 1;
        }

        let mut sorted_bridges: Vec<((String, String), usize)> = bridges.into_iter().collect();
        sorted_bridges.sort_by_key(|a| std::cmp::Reverse(a.1));
        for ((a, b), count) in sorted_bridges.into_iter().take(12) {
            let _ = writeln!(out, "  <bridge from=\"{a}\" to=\"{b}\" edges=\"{count}\"/>");
        }
        out.push_str("</communities>");
        Ok((out, emitted))
    })
}

fn cluster_name(rel_path: &str) -> String {
    let parts: Vec<&str> = rel_path.split('/').collect();
    if parts.len() <= 1 {
        "(root)".to_string()
    } else if parts.len() == 2 {
        parts[0].to_string()
    } else {
        format!("{}/{}", parts[0], parts[1])
    }
}
