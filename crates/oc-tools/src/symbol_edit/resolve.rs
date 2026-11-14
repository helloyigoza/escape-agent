//! Target resolution for `symbol_edit`: symbol/file splitting plus anchor and
//! span resolution over the native symbol index.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::code_map::{NativeSymbolLocation, find_native_symbol_locations};
use crate::util;
use crate::{ToolContext, ToolError};

pub(super) fn split_symbol_and_file(
    symbol: &str,
    path_arg: Option<&str>,
) -> (String, Option<String>) {
    if let Some(p) = path_arg {
        let stripped = symbol
            .rsplit_once(':')
            .filter(|(lhs, rhs)| {
                !lhs.is_empty()
                    && !rhs.is_empty()
                    && !rhs.contains(['/', '\\'])
                    && !lhs.starts_with('@')
                    && !symbol.contains("::")
            })
            .map_or(symbol, |(_, rhs)| rhs);
        return (stripped.to_string(), Some(p.to_string()));
    }
    if symbol.starts_with('@') || symbol.contains("::") {
        return (symbol.to_string(), None);
    }
    if let Some((file_part, sym_part)) = symbol.rsplit_once(':')
        && !file_part.is_empty()
        && !sym_part.is_empty()
        && !sym_part.contains(['/', '\\'])
    {
        return (sym_part.to_string(), Some(file_part.to_string()));
    }
    (symbol.to_string(), None)
}

pub(super) fn resolve_unique_candidate_abs(
    ctx: &ToolContext,
    root: &Path,
    scope_path: &Path,
    symbol: &str,
    path_arg: Option<&str>,
) -> Option<PathBuf> {
    if path_arg.is_some() && scope_path.is_file() {
        return Some(scope_path.to_path_buf());
    }
    if let Some(rest) = symbol.strip_prefix('@')
        && let Some((file_part, _)) = rest.rsplit_once(':')
    {
        let p = util::resolve(ctx, file_part.trim());
        if p.is_file() {
            return Some(p);
        }
        let rp = root.join(file_part.trim());
        if rp.is_file() {
            return Some(rp);
        }
    }
    let locs = find_native_symbol_locations(ctx, scope_path, symbol).ok()?;
    if locs.len() == 1 {
        return Some(locs[0].abs_path.clone());
    }
    None
}

pub(super) fn resolve_native_target(
    ctx: &ToolContext,
    root: &Path,
    scope_path: &Path,
    symbol: &str,
    path_arg: Option<&str>,
) -> Result<Vec<NativeSymbolLocation>, ToolError> {
    let query = if let Some(p) = path_arg {
        let stripped = symbol
            .rsplit_once(':')
            .filter(|(lhs, rhs)| {
                !lhs.is_empty()
                    && !rhs.is_empty()
                    && !rhs.contains(['/', '\\'])
                    && !lhs.starts_with('@')
                    && !symbol.contains("::")
            })
            .map_or(symbol, |(_, rhs)| rhs);
        let scope_target = util::resolve(ctx, p);
        let locs = find_native_symbol_locations(ctx, &scope_target, stripped)?;
        if !locs.is_empty() {
            return Ok(locs);
        }
        stripped.to_string()
    } else {
        symbol.to_string()
    };

    let mut locs = find_native_symbol_locations(ctx, scope_path, &query)?;
    if locs.is_empty() && path_arg.is_none() {
        locs = find_native_symbol_locations(ctx, root, &query)?;
    }
    Ok(locs)
}

pub(super) fn pick_single_location<'a>(
    ctx: &ToolContext,
    locs: &'a [NativeSymbolLocation],
    query: &str,
) -> Result<&'a NativeSymbolLocation, ToolError> {
    if locs.is_empty() {
        return Err(ToolError::new(format!(
            "Symbol `{query}` not found. Pass `path` or `@file:line` to target it explicitly."
        )));
    }
    if locs.len() == 1 {
        return Ok(&locs[0]);
    }

    // Prefer exact bare-name matches before rejecting ambiguity.
    let short_query = query.rsplit([':', '.']).next().unwrap_or(query).trim();
    let exact: Vec<&NativeSymbolLocation> = locs
        .iter()
        .filter(|l| l.name.eq_ignore_ascii_case(short_query))
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0]);
    }

    let candidates = if exact.is_empty() {
        locs.iter().collect::<Vec<_>>()
    } else {
        exact
    };
    let mut msg = format!(
        "Symbol `{query}` matched {} definitions; pass `path` or `@file:line` to disambiguate:\n",
        candidates.len()
    );
    for loc in candidates.iter().take(8) {
        let rel = util::relative_display(&ctx.directory, &loc.abs_path);
        let _ = writeln!(
            msg,
            "  - {}:{} [{}] {}",
            rel,
            loc.start_line,
            loc.kind.as_str(),
            loc.signature
        );
    }
    Err(ToolError::new(msg))
}
