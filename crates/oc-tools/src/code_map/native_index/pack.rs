use std::collections::BTreeMap;
use std::path::Path;

use super::super::ripwire::strip_comments_and_blank_runs;
use super::index::{
    CachedWorkspace, IndexedSymbol, path_matches_scope, relative_scope, tree_root_for,
    usize_to_f32, with_index,
};
use crate::{ToolContext, ToolError};
use std::fmt::Write as _;

/// Native `pack_task`: budgeted focal signatures + 1-hop graph neighbors + compact bodies.
pub(in crate::code_map) fn native_pack_task(
    ctx: &ToolContext,
    target_path: &Path,
    task: &str,
    max_tokens: usize,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let q_terms: Vec<String> = task
            .split_whitespace()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();

        let mut scored: Vec<(f32, usize)> = Vec::new();
        for sym in &ws.all_symbols {
            if !path_matches_scope(&sym.rel_path, scope.as_deref()) {
                continue;
            }
            let name_low = sym.sym.name.to_lowercase();
            let sig_low = sym.sym.signature.to_lowercase();
            let path_low = sym.rel_path.to_lowercase();
            let mut weight = 0.0_f32;
            for term in &q_terms {
                if name_low == *term {
                    weight += 15.0;
                } else if name_low.contains(term) {
                    weight += 8.0;
                }
                if sig_low.contains(term) {
                    weight += 3.0;
                }
                if path_low.contains(term) {
                    weight += 4.0;
                }
            }
            if weight > 0.0 {
                weight += usize_to_f32(ws.fan_in(sym.id)) * 1.2;
                scored.push((weight, sym.id));
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));

        let max_chars = max_tokens.saturating_mul(4).max(400);
        let mut out = format!("<ctx task=\"{task}\">\n<sigs>\n");
        let mut focal_ids = Vec::new();

        for (_, id) in scored.into_iter().take(limit.min(16)) {
            let s = &ws.all_symbols[id];
            let sig_line = format!(
                "  {}:{} [{}] {}\n",
                s.rel_path,
                s.start_line,
                s.sym.kind.as_str(),
                s.sym.signature
            );
            if out.len() + sig_line.len() > max_chars / 2 && !focal_ids.is_empty() {
                break;
            }
            out.push_str(&sig_line);
            focal_ids.push(id);
        }
        out.push_str("</sigs>\n<bodies>\n");

        for id in focal_ids.iter().take(4) {
            if out.len() >= max_chars {
                break;
            }
            let sym = &ws.all_symbols[*id];
            let file_path = root.join(&sym.rel_path);
            let Some(file) = ws.files_by_path.get(&file_path) else {
                continue;
            };
            let start = sym.start_line.saturating_sub(1);
            let end = sym.end_line.min(f_end(file.lines.len(), start));
            let slice = strip_comments_and_blank_runs(&file.lines[start..end].join("\n"));
            let entry = format!(
                "  <body symbol=\"{}\" file=\"{}:{}\">\n{}\n  </body>\n",
                sym.sym.name, sym.rel_path, sym.start_line, slice
            );
            if out.len() + entry.len() <= max_chars || out.len() < 400 {
                out.push_str(&entry);
            }
        }
        out.push_str("</bodies>\n</ctx>");
        Ok((out, focal_ids.len()))
    })
}

fn f_end(len: usize, start: usize) -> usize {
    len.min(start.saturating_add(40))
}

/// Partition slice computed either natively or from `ripwire`.
#[derive(Debug, Clone)]
pub(crate) struct NativePartitionSlice {
    pub(crate) index: usize,
    pub(crate) files: Vec<String>,
    pub(crate) symbols: Vec<String>,
    pub(crate) text: String,
}

type FileScoreMap<'a> = BTreeMap<String, (f32, Vec<&'a IndexedSymbol>)>;

type PartitionBucket<'a> = (f32, Vec<(String, Vec<&'a IndexedSymbol>)>);

/// Disjoint graph partitioning fallback when `ripwire` is absent.
pub(crate) fn native_partition_task(
    ctx: &ToolContext,
    target_path: &Path,
    task: &str,
    partitions: usize,
    max_tokens: usize,
) -> Result<(String, Vec<NativePartitionSlice>, String), ToolError> {
    let partitions = partitions.clamp(2, 16);
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let q_terms: Vec<String> = task
            .split_whitespace()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| t.len() >= 2)
            .collect();

        let (file_scores, mut hubs) = score_files_for_partition(ws, scope.as_deref(), &q_terms);
        hubs.sort_by_key(|a| std::cmp::Reverse(a.0));
        let core_text = render_partition_core(&hubs, max_tokens);

        let mut ranked_files: Vec<(String, f32, Vec<&IndexedSymbol>)> = file_scores
            .into_iter()
            .map(|(f, (s, syms))| (f, s, syms))
            .collect();
        ranked_files.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let slices = pack_partition_buckets(ranked_files, partitions, max_tokens);
        Ok((core_text, slices, "native".to_string()))
    })
}

fn score_files_for_partition<'a>(
    ws: &'a CachedWorkspace,
    scope: Option<&str>,
    q_terms: &[String],
) -> (FileScoreMap<'a>, Vec<(usize, &'a IndexedSymbol)>) {
    let mut file_scores: FileScoreMap<'a> = BTreeMap::new();
    let mut hubs: Vec<(usize, &IndexedSymbol)> = Vec::new();
    for sym in &ws.all_symbols {
        if !path_matches_scope(&sym.rel_path, scope) {
            continue;
        }
        let degree = ws.fan_in(sym.id) + ws.fan_out(sym.id);
        if degree >= 2 {
            hubs.push((degree, sym));
        }
        let name_low = sym.sym.name.to_lowercase();
        let path_low = sym.rel_path.to_lowercase();
        let sig_low = sym.sym.signature.to_lowercase();
        let mut rel = 0.5_f32 + usize_to_f32(degree) * 0.25;
        for term in q_terms {
            if name_low.contains(term) {
                rel += 8.0;
            }
            if sig_low.contains(term) {
                rel += 3.0;
            }
            if path_low.contains(term) {
                rel += 4.0;
            }
        }
        let entry = file_scores
            .entry(sym.rel_path.clone())
            .or_insert_with(|| (0.0, Vec::new()));
        entry.0 += rel;
        entry.1.push(sym);
    }
    (file_scores, hubs)
}

fn render_partition_core(hubs: &[(usize, &IndexedSymbol)], max_tokens: usize) -> String {
    let mut core_text = String::from("<core>\n");
    let core_budget = (max_tokens * 2).max(300);
    for (_, h) in hubs.iter().take(8) {
        let line = format!(
            "  {}:{} [{}] {}\n",
            h.rel_path,
            h.start_line,
            h.sym.kind.as_str(),
            h.sym.signature
        );
        if core_text.len() + line.len() <= core_budget {
            core_text.push_str(&line);
        }
    }
    core_text.push_str("</core>");
    core_text
}

fn pack_partition_buckets(
    ranked_files: Vec<(String, f32, Vec<&IndexedSymbol>)>,
    partitions: usize,
    max_tokens: usize,
) -> Vec<NativePartitionSlice> {
    let mut buckets: Vec<PartitionBucket<'_>> = vec![(0.0, Vec::new()); partitions];
    for (file, score, syms) in ranked_files.into_iter().take(partitions.saturating_mul(6)) {
        if let Some(target_idx) = buckets
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.0.total_cmp(&b.0))
            .map(|(i, _)| i)
        {
            buckets[target_idx].0 += score;
            buckets[target_idx].1.push((file, syms));
        }
    }

    let per_slice_chars = max_tokens.saturating_mul(4).max(300);
    let mut slices = Vec::with_capacity(partitions);
    for (idx, (_, assigned)) in buckets.into_iter().enumerate() {
        let mut files = Vec::new();
        let mut symbols = Vec::new();
        let mut text = format!("<partition i=\"{idx}\">\n");
        for (rel_file, syms) in assigned {
            files.push(rel_file.clone());
            let _ = writeln!(text, "  <f p=\"{rel_file}\">");
            for sym in syms.into_iter().take(8) {
                symbols.push(sym.sym.name.clone());
                let row = format!(
                    "    L{:<4} [{}] {}\n",
                    sym.start_line,
                    sym.sym.kind.as_str(),
                    sym.sym.signature
                );
                if text.len() + row.len() <= per_slice_chars {
                    text.push_str(&row);
                }
            }
            text.push_str("  </f>\n");
        }
        text.push_str("</partition>");
        slices.push(NativePartitionSlice {
            index: idx,
            files,
            symbols,
            text,
        });
    }
    slices
}
