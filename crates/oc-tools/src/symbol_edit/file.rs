//! File snapshots and BOM/EOL-preserving text reconstruction shared by the
//! `symbol_edit` engines.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ToolError;
use crate::read::MAX_FILE_BYTES;
use crate::util;

#[derive(Debug, Clone)]
pub(super) struct FileSnapshot {
    pub(super) abs_path: PathBuf,
    pub(super) display_rel: String,
    pub(super) bom: bool,
    pub(super) crlf: bool,
    pub(super) clean_lf: String,
}

pub(super) fn read_snapshot(base_dir: &Path, abs_path: &Path) -> Result<FileSnapshot, ToolError> {
    if let Ok(meta) = fs::metadata(abs_path)
        && meta.len() > MAX_FILE_BYTES
    {
        return Err(ToolError::new(format!(
            "File too large for symbol edit: {} exceeds {} bytes",
            util::relative_display(base_dir, abs_path),
            MAX_FILE_BYTES
        )));
    }
    let bytes = fs::read(abs_path)
        .map_err(|e| ToolError::new(format!("Failed to read {}: {e}", abs_path.display())))?;
    let (bom, decoded) = util::decode_bytes(&bytes);
    let crlf = decoded.contains("\r\n");
    let clean_lf = decoded.replace("\r\n", "\n");
    Ok(FileSnapshot {
        abs_path: abs_path.to_path_buf(),
        display_rel: util::relative_display(base_dir, abs_path),
        bom,
        crlf,
        clean_lf,
    })
}

pub(super) fn format_with_bom_eol(lf_text: &str, bom: bool, crlf: bool) -> String {
    let eol_text = if crlf {
        lf_text.replace('\n', "\r\n")
    } else {
        lf_text.to_string()
    };
    util::join_bom(&eol_text, bom)
}
