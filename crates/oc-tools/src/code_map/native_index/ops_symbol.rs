use std::fs;
use std::path::{Path, PathBuf};

use super::super::SymbolKind;
use super::super::ripwire::strip_comments_and_blank_runs;
use super::index::{relative_scope, tree_root_for, with_index};
use crate::{ToolContext, ToolError};
use std::fmt::Write as _;

/// Native `symbol` lookup enriched with 1-hop callers and callees.
pub(in crate::code_map) fn native_symbol(
    ctx: &ToolContext,
    target_path: &Path,
    query: &str,
    limit: usize,
) -> Result<(String, usize), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let ids = ws.find_matching_symbols(scope.as_deref(), query);
        let mut lines = Vec::new();
        for &id in ids.iter().take(limit) {
            let s = &ws.all_symbols[id];
            let incoming: Vec<&str> = ws.callers[id]
                .iter()
                .take(4)
                .map(|&cid| ws.all_symbols[cid].sym.name.as_str())
                .collect();
            let outgoing: Vec<&str> = ws.callees[id]
                .iter()
                .take(4)
                .map(|&cid| ws.all_symbols[cid].sym.name.as_str())
                .collect();

            let mut line = format!(
                "{}:{} [{}] {} | handle=@{}:{}",
                s.rel_path,
                s.start_line,
                s.sym.kind.as_str(),
                s.sym.signature,
                s.rel_path,
                s.start_line,
            );
            if !incoming.is_empty() {
                let _ = write!(line, " | callers:[{}]", incoming.join(","));
            }
            if !outgoing.is_empty() {
                let _ = write!(line, " | calls:[{}]", outgoing.join(","));
            }
            lines.push(line);
        }
        let count = lines.len();
        let output = if lines.is_empty() {
            format!("No symbol definitions found matching '{query}'.")
        } else {
            lines.join("\n")
        };
        Ok((output, count))
    })
}

/// Native `expand` operation: extract full or sliced body with optional compression.
pub(in crate::code_map) fn native_expand(
    ctx: &ToolContext,
    target_path: &Path,
    query: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    compress: bool,
) -> Result<(String, String), ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let ids = ws.find_matching_symbols(scope.as_deref(), query);
        let Some(&sym_id) = ids.first() else {
            return Err(ToolError::new(format!(
                "No symbol or handle found for expand target '{query}'."
            )));
        };
        let sym = &ws.all_symbols[sym_id];
        let file_path = root.join(&sym.rel_path);
        let file = ws
            .files_by_path
            .get(&file_path)
            .ok_or_else(|| ToolError::new(format!("File not indexed: {}", sym.rel_path)))?;

        let body_start = sym.start_line;
        let refined_end =
            compute_refined_end_line(&file.lines.join("\n"), &file.ext, body_start, sym.end_line);
        let body_end = refined_end.min(file.lines.len());
        let slice_start = start_line
            .map_or(body_start, |off| {
                body_start.saturating_add(off.saturating_sub(1))
            })
            .max(1)
            .min(file.lines.len());
        let slice_end = end_line
            .map_or(body_end, |off| {
                body_start.saturating_add(off.saturating_sub(1))
            })
            .min(file.lines.len())
            .max(slice_start);

        let raw_slice = file.lines[slice_start - 1..slice_end].join("\n");
        let body = if compress {
            strip_comments_and_blank_runs(&raw_slice)
        } else {
            raw_slice
        };

        let out = format!(
            "Symbol: {} [{}] ({}:{}..{})\n```{}\n{}\n```",
            sym.sym.name,
            sym.sym.kind.as_str(),
            sym.rel_path,
            slice_start,
            slice_end,
            file.ext,
            body
        );
        Ok((out, sym.rel_path.clone()))
    })
}

/// Resolved 1-based `(file_path, start_line, end_line, name, kind)` for a native
/// symbol lookup, used by LSP precision anchoring and atomic native symbol edits.
#[derive(Debug, Clone)]
pub(crate) struct NativeSymbolLocation {
    pub(crate) abs_path: PathBuf,
    pub(crate) name: String,
    pub(crate) kind: SymbolKind,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) signature: String,
}

/// Locate a symbol's declaration `(abs_path, 1-based line, 1-based column)` for
/// LSP precision queries.
pub(in crate::code_map) fn locate_symbol_line_col(
    ctx: &ToolContext,
    target_path: &Path,
    query: &str,
) -> Option<(PathBuf, u32, u32)> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let ids = ws.find_matching_symbols(scope.as_deref(), query);
        let Some(&id) = ids.first() else {
            return Ok(None);
        };
        let sym = &ws.all_symbols[id];
        let abs = root.join(&sym.rel_path);
        let line_1 = sym.start_line.max(1);
        let col_1 = ws
            .files_by_path
            .get(&abs)
            .and_then(|f| f.lines.get(line_1 - 1))
            .and_then(|line_text| {
                let short = sym
                    .sym
                    .name
                    .rsplit([':', '.'])
                    .next()
                    .unwrap_or(&sym.sym.name);
                line_text.find(short).map(|byte_off| {
                    line_text[..byte_off]
                        .encode_utf16()
                        .count()
                        .saturating_add(1)
                })
            })
            .unwrap_or(1);
        Ok(Some((
            abs,
            u32::try_from(line_1).unwrap_or(u32::MAX),
            u32::try_from(col_1).unwrap_or(1),
        )))
    })
    .ok()
    .flatten()
}

/// Resolve all matching definitions for a symbol query (`symbol`, `file:symbol`,
/// or `@FILE:LINE`), optionally confined to `target_file`.
pub(crate) fn find_native_symbol_locations(
    ctx: &ToolContext,
    target_path: &Path,
    query: &str,
) -> Result<Vec<NativeSymbolLocation>, ToolError> {
    let root = tree_root_for(ctx, target_path);
    with_index(ctx, &root, |ws| {
        let scope = relative_scope(&root, target_path);
        let ids = ws.find_matching_symbols(scope.as_deref(), query);
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let sym = &ws.all_symbols[id];
            let abs_path = root.join(&sym.rel_path);
            let refined_end = fs::read_to_string(&abs_path).map_or(sym.end_line, |src| {
                let ext = abs_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                compute_refined_end_line(&src, ext, sym.start_line, sym.end_line)
            });
            out.push(NativeSymbolLocation {
                abs_path,
                name: sym.sym.name.clone(),
                kind: sym.sym.kind,
                start_line: sym.start_line,
                end_line: refined_end,
                signature: sym.sym.signature.clone(),
            });
        }
        Ok(out)
    })
}

/// String/comment-aware symbol end line calculation for atomic fallback edits.
fn compute_refined_end_line(
    source: &str,
    ext: &str,
    start_line: usize,
    fallback_end: usize,
) -> usize {
    if matches!(ext.to_ascii_lowercase().as_str(), "py" | "md" | "markdown") {
        return fallback_end;
    }
    let Some(start_byte) = line_byte_offset(source, start_line) else {
        return fallback_end;
    };
    let bytes = source.as_bytes();
    let mut i = start_byte;
    let mut depth: i32 = 0;
    let mut saw_open = false;
    let mut in_line_comment = false;
    let mut block_comment_depth: usize = 0;
    let mut in_string: Option<u8> = None;
    let mut current_line = start_line;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            current_line += 1;
            in_line_comment = false;
            i += 1;
            continue;
        }
        if in_line_comment {
            i += 1;
            continue;
        }
        if block_comment_depth > 0 {
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                block_comment_depth += 1;
                i += 2;
                continue;
            }
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                block_comment_depth -= 1;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(quote) = in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'*' {
                block_comment_depth = 1;
                i += 2;
                continue;
            }
        }
        if ext == "rs" && b == b'\'' {
            if i + 3 < bytes.len() && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
                i += 4;
                continue;
            }
            if i + 2 < bytes.len()
                && bytes[i + 1] != b'\\'
                && bytes[i + 1] != b'\''
                && bytes[i + 1] != b'\n'
                && bytes[i + 2] == b'\''
            {
                i += 3;
                continue;
            }
        }
        if b == b'"' || (b == b'\'' && ext != "rs") {
            in_string = Some(b);
            i += 1;
            continue;
        }
        if b == b'{' {
            depth += 1;
            saw_open = true;
        } else if b == b'}' {
            depth = depth.saturating_sub(1);
            if saw_open && depth == 0 {
                return current_line;
            }
        } else if b == b';' && !saw_open && depth == 0 {
            return current_line;
        }
        i += 1;
    }
    fallback_end
}

fn line_byte_offset(source: &str, line_1: usize) -> Option<usize> {
    if line_1 <= 1 {
        return Some(0);
    }
    let mut cur = 1usize;
    for (idx, b) in source.bytes().enumerate() {
        if b == b'\n' {
            cur += 1;
            if cur == line_1 {
                return Some(idx + 1);
            }
        }
    }
    None
}
