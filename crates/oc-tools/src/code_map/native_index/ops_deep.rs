use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use super::super::SymbolKind;
use super::index::{path_matches_scope, relative_scope, tree_root_for, with_index};
use super::parse::extract_call_idents;
use crate::{ToolContext, ToolError};
use std::fmt::Write as _;

/// Native `lego`: find interface/trait contract and all implementing types.
pub(in crate::code_map) fn native_lego(
    ctx: &ToolContext,
    target_path: &Path,
    type_name: &str,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    let needle = type_name.trim();
    let short_needle = needle.rsplit([':', '.']).next().unwrap_or(needle);
    let needle_low = short_needle.to_lowercase();

    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let mut def_lines = Vec::new();
        let mut impls: BTreeMap<String, (String, usize, String)> = BTreeMap::new();

        for path in &ws.file_order {
            let Some(file) = ws.files_by_path.get(path) else {
                continue;
            };
            if !path_matches_scope(&file.rel_path, scope.as_deref()) {
                continue;
            }
            for sym in &file.symbols {
                if sym.sym.name.eq_ignore_ascii_case(short_needle)
                    && matches!(
                        sym.sym.kind,
                        SymbolKind::Trait
                            | SymbolKind::Interface
                            | SymbolKind::Class
                            | SymbolKind::Struct
                    )
                {
                    def_lines.push(format!(
                        "{}:{} [{}] {}",
                        sym.rel_path,
                        sym.start_line,
                        sym.sym.kind.as_str(),
                        sym.sym.signature
                    ));
                }
                if sym.sym.kind == SymbolKind::Class {
                    let name_low = sym.sym.name.to_lowercase();
                    if name_low.starts_with(&format!("{needle_low} for ")) {
                        let impl_name = sym
                            .sym
                            .name
                            .split_once(" for ")
                            .map_or(sym.sym.name.as_str(), |(_, rhs)| rhs.trim())
                            .to_string();
                        impls.entry(impl_name.clone()).or_insert_with(|| {
                            (
                                sym.rel_path.clone(),
                                sym.start_line,
                                sym.sym.signature.clone(),
                            )
                        });
                    }
                }
            }

            for (idx, line) in file.lines.iter().enumerate() {
                if !line.contains(short_needle) {
                    continue;
                }
                let trimmed = line.trim();
                let is_impl_line = trimmed.starts_with("impl ")
                    || trimmed.starts_with("impl<")
                    || trimmed.contains(&format!("implements {short_needle}"))
                    || trimmed.contains(&format!("extends {short_needle}"))
                    || (trimmed.starts_with("class ")
                        && trimmed.contains(&format!("({short_needle}")));
                if is_impl_line {
                    let key = format!("{}:{}", file.rel_path, idx + 1);
                    impls
                        .entry(key)
                        .or_insert_with(|| (file.rel_path.clone(), idx + 1, trimmed.to_string()));
                }
            }
        }

        let total = impls.len();
        let mut out = format!("Interface `{short_needle}` (implementors: {total}):\n");
        if def_lines.is_empty() {
            let _ = writeln!(out, "  definition: (not uniquely indexed in scope)");
        } else {
            for d in def_lines.iter().take(4) {
                let _ = writeln!(out, "  definition: {d}");
            }
        }
        for (_, (rel, line_no, sig)) in impls.into_iter().take(limit) {
            let _ = writeln!(out, "  - {rel}:{line_no} {sig}");
        }
        Ok((out, total))
    })
}

/// Native `slice`: intra-procedural def-use variable slice inside one function.
pub(in crate::code_map) fn native_slice(
    ctx: &ToolContext,
    target_path: &Path,
    sym_query: &str,
    var: Option<&str>,
    flow: Option<&str>,
    depth: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let (sym_part, inline_var) = sym_query
            .rsplit_once(':')
            .filter(|(lhs, rhs)| {
                !lhs.is_empty() && !rhs.is_empty() && rhs.parse::<usize>().is_err()
            })
            .map_or((sym_query, None), |(l, r)| (l, Some(r)));
        let target_var = var.or(inline_var).map(str::trim).filter(|s| !s.is_empty());

        let ids = ws.find_matching_symbols(scope.as_deref(), sym_part);
        let Some(&id) = ids.first() else {
            return Ok((format!("Symbol `{sym_part}` not found for slice."), 0));
        };
        let sym = &ws.all_symbols[id];
        let abs = root.join(&sym.rel_path);
        let Some(file) = ws.files_by_path.get(&abs) else {
            return Ok((format!("Source unavailable for {}", sym.rel_path), 0));
        };
        let start = sym.start_line.saturating_sub(1);
        let end = sym.end_line.min(file.lines.len());
        let body_lines = &file.lines[start..end];

        let mut vars_seen = BTreeSet::new();
        for line in body_lines {
            for ident in extract_call_idents(line) {
                if ident.chars().next().is_some_and(char::is_lowercase) {
                    vars_seen.insert(ident);
                }
            }
        }

        let Some(var_name) = target_var else {
            let list: Vec<String> = vars_seen.into_iter().take(32).collect();
            return Ok((
                format!(
                    "Sliceable locals in `{}` ({}:{}..{}):\n  {}",
                    sym.sym.name,
                    sym.rel_path,
                    sym.start_line,
                    sym.end_line,
                    list.join(", ")
                ),
                list.len(),
            ));
        };

        let mut rows = Vec::new();
        for (offset, line) in body_lines.iter().enumerate() {
            if contains_ident_token(line, var_name) {
                let line_no = sym.start_line + offset;
                let trimmed = line.trim();
                let is_def = trimmed.starts_with(&format!("let {var_name}"))
                    || trimmed.starts_with(&format!("let mut {var_name}"))
                    || trimmed.contains(&format!("{var_name} = "))
                    || trimmed.contains(&format!("{var_name} :="));
                let kind = if is_def && offset > 0 {
                    "def"
                } else if offset == 0 {
                    "param"
                } else {
                    "use"
                };
                rows.push(format!("  L{line_no:<4} [{kind}] {trimmed}"));
            }
        }
        let flow_label = flow.unwrap_or("linear");
        let capped = rows
            .into_iter()
            .take(depth.saturating_mul(6).max(20))
            .collect::<Vec<_>>();
        let count = capped.len();
        let out = format!(
            "Slice `{}:{}` ({}:{}, flow={flow_label}, hits={count}):\n{}",
            sym.sym.name,
            var_name,
            sym.rel_path,
            sym.start_line,
            capped.join("\n")
        );
        Ok((out, count))
    })
}

fn contains_ident_token(line: &str, needle: &str) -> bool {
    extract_call_idents(line).iter().any(|tok| tok == needle)
}

/// Native `doc_drift`: verify backticked path/symbol anchors in markdown docs.
pub(in crate::code_map) fn native_doc_drift(
    ctx: &ToolContext,
    target_path: &Path,
    filter: Option<&str>,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let mut indexed_paths: HashSet<String> = HashSet::new();
        for p in &ws.file_order {
            if let Some(f) = ws.files_by_path.get(p) {
                indexed_paths.insert(f.rel_path.clone());
            }
        }

        let mut findings = Vec::new();
        for p in &ws.file_order {
            let Some(file) = ws.files_by_path.get(p) else {
                continue;
            };
            if !matches!(file.ext.to_ascii_lowercase().as_str(), "md" | "markdown") {
                continue;
            }
            if !path_matches_scope(&file.rel_path, scope.as_deref()) {
                continue;
            }
            if let Some(sub) = filter
                && !file.rel_path.contains(sub)
            {
                continue;
            }
            let mut in_fence = false;
            for (idx, line) in file.lines.iter().enumerate() {
                if line.trim_start().starts_with("```") {
                    in_fence = !in_fence;
                    continue;
                }
                if in_fence {
                    continue;
                }
                for token in extract_backticked_tokens(line) {
                    if looks_like_repo_path(&token)
                        && !root.join(&token).exists()
                        && !indexed_paths.iter().any(|ip| ip.ends_with(&token))
                    {
                        findings.push(format!(
                            "  {}:{} missing path `{token}`",
                            file.rel_path,
                            idx + 1
                        ));
                    }
                }
            }
        }

        let total = findings.len();
        let mut out = format!("Doc drift check ({total} missing path anchors):\n");
        for row in findings.into_iter().take(limit) {
            out.push_str(&row);
            out.push('\n');
        }
        Ok((out, total))
    })
}

fn extract_backticked_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else {
            break;
        };
        let tok = rest[..close].trim();
        if !tok.is_empty() && !tok.contains(' ') {
            tokens.push(tok.to_string());
        }
        rest = &rest[close + 1..];
    }
    tokens
}

fn looks_like_repo_path(tok: &str) -> bool {
    (tok.contains('/') && !tok.starts_with("http") && !tok.starts_with('/') && !tok.contains('*'))
        && matches!(
            Path::new(tok).extension().and_then(|e| e.to_str()),
            Some("rs" | "ts" | "tsx" | "js" | "py" | "go" | "md" | "toml" | "json")
        )
}

/// Native `flags`: detect feature/env/cfg gates across workspace source.
pub(in crate::code_map) fn native_flags(
    ctx: &ToolContext,
    target_path: &Path,
    filter: Option<&str>,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let filter_low = filter.map(str::to_lowercase);
        let mut hits = Vec::new();
        for p in &ws.file_order {
            let Some(file) = ws.files_by_path.get(p) else {
                continue;
            };
            if !path_matches_scope(&file.rel_path, scope.as_deref()) {
                continue;
            }
            for (idx, line) in file.lines.iter().enumerate() {
                let trimmed = line.trim();
                let is_gate = trimmed.contains("#[cfg(")
                    || trimmed.contains("env::var")
                    || trimmed.contains("process.env.")
                    || trimmed.contains("os.environ");
                if !is_gate {
                    continue;
                }
                if let Some(ref f) = filter_low
                    && !trimmed.to_lowercase().contains(f)
                    && !file.rel_path.to_lowercase().contains(f)
                {
                    continue;
                }
                hits.push(format!("  {}:{} {}", file.rel_path, idx + 1, trimmed));
            }
        }
        let total = hits.len();
        let mut out = format!("Feature & env gates ({total} hits):\n");
        for row in hits.into_iter().take(limit) {
            out.push_str(&row);
            out.push('\n');
        }
        Ok((out, total))
    })
}
