use std::path::Path;

pub(super) fn spill_tool_output(directory: &str, call_id: &str, text: &str) -> Option<String> {
    let sanitized: String = call_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let filename = if sanitized.is_empty() {
        format!("tool-{}.txt", oc_types::now_millis())
    } else {
        format!("{sanitized}.txt")
    };
    let primary_dir = Path::new(directory).join(".opencode").join("tool-output");
    if std::fs::create_dir_all(&primary_dir).is_ok() {
        let target = primary_dir.join(&filename);
        if std::fs::write(&target, text).is_ok() {
            return Some(target.to_string_lossy().into_owned());
        }
    }
    let fallback_dir = std::env::temp_dir().join("opencode-tool-output");
    std::fs::create_dir_all(&fallback_dir).ok()?;
    let target = fallback_dir.join(&filename);
    std::fs::write(&target, text).ok()?;
    Some(target.to_string_lossy().into_owned())
}

pub(super) fn char_boundary_end(text: &str, max_chars: usize) -> usize {
    text.char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(idx, _)| idx)
}

pub(super) fn char_boundary_tail(text: &str, tail_chars: usize) -> usize {
    let total = text.chars().count();
    let skip = total.saturating_sub(tail_chars);
    text.char_indices().nth(skip).map_or(0, |(idx, _)| idx)
}
