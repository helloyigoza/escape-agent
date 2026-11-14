//! The ordered document loader: global directory, explicit file, ancestors,
//! project directories, inline content and the managed system directory.

use std::path::Path;

use serde_json::Value;

use crate::discovery::{self, Discovery, Env};
use crate::{ConfigEntry, Diagnostic, DiagnosticKind, normalize, parse_jsonc, substitute};

pub(super) struct Loader<'a> {
    discovery: &'a Discovery,
    env: &'a Env,
    pub(super) entries: Vec<ConfigEntry>,
    pub(super) documents: Vec<Value>,
    pub(super) diagnostics: Vec<Diagnostic>,
}

impl<'a> Loader<'a> {
    pub(super) fn new(discovery: &'a Discovery, env: &'a Env) -> Self {
        Self {
            discovery,
            env,
            entries: Vec::new(),
            documents: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub(super) fn load_all(&mut self) {
        // 1. Global directory (always represented, files when present).
        let global = self.discovery.global.clone();
        self.load_directory(&global);

        // 2. Explicit `OPENCODE_CONFIG` file.
        if let Some(path) = self.discovery.explicit.clone()
            && path.is_file()
        {
            self.load_file(&path);
        }

        // 3. Ancestor direct files, outermost first.
        let direct = self.discovery.direct.clone();
        for path in &direct {
            self.load_file(path);
        }

        // 4. Project `.opencode` directories, outermost first.
        let project = self.discovery.project.clone();
        for directory in &project {
            self.load_directory(directory);
        }

        // 5. Inline `OPENCODE_CONFIG_CONTENT`.
        if let Some(content) = self.discovery.content.clone()
            && !content.trim().is_empty()
        {
            let location = self.discovery.location.clone();
            self.parse_and_push(Some("OPENCODE_CONFIG_CONTENT"), &content, &location, None);
        }

        // 6. Managed system directory, highest priority.
        if self.discovery.system_present {
            let system = self.discovery.system.clone();
            self.load_directory(&system);
        }
    }

    fn load_directory(&mut self, directory: &Path) {
        for name in discovery::NAMES {
            let path = directory.join(name);
            if path.is_file() {
                self.load_file(&path);
            }
        }
        self.entries.push(ConfigEntry::directory(
            directory.to_string_lossy().into_owned(),
        ));
    }

    fn load_file(&mut self, path: &Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.diagnostics.push(Diagnostic::new(
                    Some(&path.to_string_lossy()),
                    DiagnosticKind::Io,
                    "$",
                    format!("failed to read configuration: {error}"),
                ));
                return;
            }
        };
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let source = path.to_string_lossy().into_owned();
        self.parse_and_push(Some(&source), &text, dir, Some(path));
    }

    fn parse_and_push(
        &mut self,
        source: Option<&str>,
        text: &str,
        dir: &Path,
        path: Option<&Path>,
    ) {
        let env = self.env.clone();
        let lookup = |name: &str| env.get(name);
        let home = self.discovery.home.clone();
        let (text, missing) = substitute::substitute(text, dir, &home, &lookup);
        for file in missing {
            self.diagnostics.push(Diagnostic::new(
                source,
                DiagnosticKind::Io,
                "$",
                format!(
                    "file reference {} is missing or unreadable: {}",
                    file.reference,
                    file.path.to_string_lossy()
                ),
            ));
        }
        let parsed = match parse_jsonc(&text) {
            Ok(value) => value,
            Err(error) => {
                self.diagnostics.push(Diagnostic::new(
                    source,
                    DiagnosticKind::Invalid,
                    "$",
                    format!("rejected malformed JSON or JSONC document: {error}"),
                ));
                return;
            }
        };
        let (info, diagnostics) = normalize::normalize(source, parsed);
        self.diagnostics.extend(diagnostics);
        if let Some(info) = info {
            self.documents.push(info.clone());
            self.entries.push(ConfigEntry::document(
                path.map(|path| path.to_string_lossy().into_owned()),
                info,
            ));
        }
    }
}
