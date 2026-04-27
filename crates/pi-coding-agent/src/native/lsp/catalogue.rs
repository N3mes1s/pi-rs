//! Default language-server catalogue (D1).
//!
//! Maps file extensions to the LSP server we'd spawn for that language.
//! Used by upstream pi's `native/lsp/catalogue.ts`. Each entry lists the
//! command + args; if the binary isn't on `$PATH` the LSP module skips
//! that language at runtime (no error).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageEntry {
    /// Stable identifier (e.g. `rust`, `typescript`).
    pub language: &'static str,
    /// File extensions handled (without leading dot).
    pub extensions: &'static [&'static str],
    /// Server command + args.
    pub command: &'static [&'static str],
}

/// The default catalogue. Mirrors oh-my-pi's defaults so users opting
/// in to LSP get useful coverage without any config.
pub const DEFAULT_CATALOGUE: &[LanguageEntry] = &[
    LanguageEntry {
        language: "rust",
        extensions: &["rs"],
        command: &["rust-analyzer"],
    },
    LanguageEntry {
        language: "typescript",
        extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
        command: &["typescript-language-server", "--stdio"],
    },
    LanguageEntry {
        language: "python",
        extensions: &["py", "pyi"],
        command: &["pyright-langserver", "--stdio"],
    },
    LanguageEntry {
        language: "go",
        extensions: &["go"],
        command: &["gopls"],
    },
    LanguageEntry {
        language: "ruby",
        extensions: &["rb"],
        command: &["solargraph", "stdio"],
    },
    LanguageEntry {
        language: "c",
        extensions: &["c", "h"],
        command: &["clangd"],
    },
    LanguageEntry {
        language: "cpp",
        extensions: &["cpp", "cc", "cxx", "hpp", "hh"],
        command: &["clangd"],
    },
    LanguageEntry {
        language: "json",
        extensions: &["json"],
        command: &["vscode-json-language-server", "--stdio"],
    },
    LanguageEntry {
        language: "yaml",
        extensions: &["yaml", "yml"],
        command: &["yaml-language-server", "--stdio"],
    },
    LanguageEntry {
        language: "lua",
        extensions: &["lua"],
        command: &["lua-language-server"],
    },
    LanguageEntry {
        language: "bash",
        extensions: &["sh", "bash"],
        command: &["bash-language-server", "start"],
    },
];

/// Look up a language entry by file extension (case-insensitive,
/// without the leading dot).
pub fn language_for_extension(ext: &str) -> Option<&'static LanguageEntry> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    DEFAULT_CATALOGUE
        .iter()
        .find(|e| e.extensions.iter().any(|x| *x == ext.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn rust_extension_resolves_to_rust_analyzer() {
        let e = language_for_extension("rs").expect("rs handled");
        assert_eq!(e.language, "rust");
        assert_eq!(e.command, &["rust-analyzer"]);
    }

    #[test]
    fn typescript_extensions_share_one_server() {
        for ext in &["ts", "tsx", "js", "jsx"] {
            let e = language_for_extension(ext).expect("ts variant handled");
            assert_eq!(e.language, "typescript");
        }
    }

    #[test]
    fn lookup_is_case_insensitive_and_strips_leading_dot() {
        assert_eq!(language_for_extension("RS").map(|e| e.language), Some("rust"));
        assert_eq!(language_for_extension(".rs").map(|e| e.language), Some("rust"));
        assert_eq!(language_for_extension(".TSX").map(|e| e.language), Some("typescript"));
    }

    #[test]
    fn lookup_returns_none_for_unknown_extension() {
        assert!(language_for_extension("xyz").is_none());
        assert!(language_for_extension("").is_none());
    }

    #[test]
    fn catalogue_languages_are_unique() {
        let mut seen: HashSet<&str> = HashSet::new();
        for e in DEFAULT_CATALOGUE {
            assert!(seen.insert(e.language), "duplicate language: {}", e.language);
        }
    }

    #[test]
    fn every_extension_resolves_back_to_its_entry() {
        for entry in DEFAULT_CATALOGUE {
            for ext in entry.extensions {
                let resolved = language_for_extension(ext).expect("resolves");
                // The first matching entry wins; assert that it shares the
                // command (clangd is registered twice, c and cpp, which is
                // intentional — the test must accept either).
                assert_eq!(resolved.command, entry.command);
            }
        }
    }
}
