use std::collections::HashSet;
use std::path::Path;

use super::super::files::{read_bounded_file, relative_to};
use super::super::parsers::parse_symbols;
use super::index::{FileStamp, IndexedFile, IndexedSymbol};
use crate::ToolError;

/// Maximum lines captured when inferring a single symbol body end line.
const MAX_SYMBOL_SPAN_LINES: usize = 300;

/// Common language keywords ignored when building native call-graph edges.
const KEYWORDS: &[&str] = &[
    "fn",
    "pub",
    "let",
    "mut",
    "const",
    "static",
    "struct",
    "enum",
    "trait",
    "impl",
    "for",
    "loop",
    "while",
    "if",
    "else",
    "match",
    "return",
    "self",
    "Self",
    "super",
    "crate",
    "mod",
    "use",
    "as",
    "in",
    "async",
    "await",
    "move",
    "type",
    "where",
    "true",
    "false",
    "None",
    "Some",
    "Ok",
    "Err",
    "String",
    "Vec",
    "Option",
    "Result",
    "bool",
    "usize",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "i32",
    "i64",
    "f32",
    "f64",
    "str",
    "def",
    "class",
    "import",
    "from",
    "function",
    "export",
    "default",
    "interface",
    "const",
    "var",
    "new",
    "this",
    "null",
    "undefined",
    "void",
    "func",
    "package",
    "defer",
    "go",
];

pub(super) fn build_indexed_file(
    root: &Path,
    path: &Path,
    stamp: FileStamp,
) -> Result<IndexedFile, ToolError> {
    let content = read_bounded_file(path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    let rel_path = relative_to(root, path).replace('\\', "/");
    let lines: Vec<String> = content.lines().map(ToString::to_string).collect();
    let parsed = parse_symbols(&content, &ext);

    let mut symbols = Vec::with_capacity(parsed.len());
    let mut referenced_idents = Vec::with_capacity(parsed.len());

    for sym in parsed {
        let start_line = sym.line.max(1);
        let next_start = usize::MAX;
        let end_line = compute_symbol_end_line(&lines, &ext, start_line, next_start);
        symbols.push(IndexedSymbol {
            id: 0,
            file_idx: 0,
            rel_path: rel_path.clone(),
            sym,
            start_line,
            end_line,
        });
    }

    // Refine Python/Markdown spans using following sibling start lines.
    if matches!(ext.to_lowercase().as_str(), "md" | "markdown") {
        for idx in 0..symbols.len() {
            let next_line = symbols
                .get(idx + 1)
                .map_or(lines.len().saturating_add(1), |s| s.start_line);
            symbols[idx].end_line = next_line.saturating_sub(1).max(symbols[idx].start_line);
        }
    }

    for sym in &symbols {
        let start_idx = sym.start_line.saturating_sub(1);
        let end_idx = sym.end_line.min(lines.len());
        let mut seen = HashSet::new();
        let mut idents = Vec::new();
        for line in lines.get(start_idx..end_idx).unwrap_or(&[]) {
            for tok in extract_call_idents(line) {
                if seen.insert(tok.clone()) {
                    idents.push(tok);
                }
            }
        }
        referenced_idents.push(idents);
    }

    Ok(IndexedFile {
        path: path.to_path_buf(),
        rel_path,
        ext,
        stamp,
        lines,
        symbols,
        referenced_idents,
    })
}

fn compute_symbol_end_line(
    lines: &[String],
    ext: &str,
    start_line_1: usize,
    _next_line_1: usize,
) -> usize {
    let total = lines.len();
    if start_line_1 == 0 || start_line_1 > total {
        return start_line_1;
    }
    let start_idx = start_line_1 - 1;
    let limit_idx = (start_idx + MAX_SYMBOL_SPAN_LINES).min(total);
    let ext_low = ext.to_lowercase();

    if ext_low == "py" {
        let base_indent = lines[start_idx]
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .count();
        let mut last_non_blank = start_line_1;
        for (offset, line) in lines[start_idx + 1..limit_idx].iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = line
                .bytes()
                .take_while(|b| *b == b' ' || *b == b'\t')
                .count();
            if indent <= base_indent {
                break;
            }
            last_non_blank = start_line_1 + 1 + offset;
        }
        return last_non_blank;
    }

    // Brace / semicolon scan for C-family, Rust, TS/JS, Go.
    let mut depth: i32 = 0;
    let mut saw_open = false;
    for (offset, line) in lines[start_idx..limit_idx].iter().enumerate() {
        let code = line.split("//").next().unwrap_or(line);
        for ch in code.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' => {
                    depth = depth.saturating_sub(1);
                    if saw_open && depth == 0 {
                        return start_line_1 + offset;
                    }
                }
                ';' if !saw_open && depth == 0 => {
                    return start_line_1 + offset;
                }
                _ => {}
            }
        }
    }
    (start_line_1 + 12).min(total)
}

pub(super) fn extract_call_idents(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let ident = &line[start..i];
            if ident.len() >= 3 && !KEYWORDS.contains(&ident) {
                out.push(ident.to_string());
            }
        } else {
            i += 1;
        }
    }
    out
}
