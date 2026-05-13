//! The 11 operations the LSP module exposes.
//!
//! These mirror upstream pi's `native/lsp/ops.ts`:
//!
//! ```text
//! diagnostics       — pull diagnostics for a file
//! definition        — go-to-definition at (file, line, col)
//! type_definition   — go-to-type-definition
//! implementation    — list implementations of a symbol
//! references        — find all references
//! hover             — hover info
//! symbols           — file-level symbol outline
//! rename            — rename symbol across the workspace
//! code_actions      — quick-fixes / refactors at a position
//! status            — which servers are currently running
//! reload            — restart a server (pickup config changes)
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspOp {
    Diagnostics,
    Definition,
    TypeDefinition,
    Implementation,
    References,
    Hover,
    Symbols,
    Rename,
    CodeActions,
    Status,
    Reload,
}

impl LspOp {
    pub const ALL: &'static [LspOp] = &[
        LspOp::Diagnostics,
        LspOp::Definition,
        LspOp::TypeDefinition,
        LspOp::Implementation,
        LspOp::References,
        LspOp::Hover,
        LspOp::Symbols,
        LspOp::Rename,
        LspOp::CodeActions,
        LspOp::Status,
        LspOp::Reload,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LspOp::Diagnostics => "diagnostics",
            LspOp::Definition => "definition",
            LspOp::TypeDefinition => "type_definition",
            LspOp::Implementation => "implementation",
            LspOp::References => "references",
            LspOp::Hover => "hover",
            LspOp::Symbols => "symbols",
            LspOp::Rename => "rename",
            LspOp::CodeActions => "code_actions",
            LspOp::Status => "status",
            LspOp::Reload => "reload",
        }
    }

    pub fn parse(s: &str) -> Option<LspOp> {
        Self::ALL.iter().copied().find(|o| o.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_count_is_eleven() {
        assert_eq!(LspOp::ALL.len(), 11);
    }

    #[test]
    fn round_trip_via_as_str_parse() {
        for op in LspOp::ALL {
            let s = op.as_str();
            assert_eq!(LspOp::parse(s), Some(*op));
        }
    }

    #[test]
    fn parse_rejects_unknown_op_names() {
        assert!(LspOp::parse("oops").is_none());
        assert!(LspOp::parse("").is_none());
        assert!(LspOp::parse("DIAGNOSTICS").is_none());
    }

    #[test]
    fn json_round_trip_uses_snake_case() {
        let s = serde_json::to_string(&LspOp::TypeDefinition).unwrap();
        assert_eq!(s, "\"type_definition\"");
        let back: LspOp = serde_json::from_str(&s).unwrap();
        assert_eq!(back, LspOp::TypeDefinition);
    }
}
