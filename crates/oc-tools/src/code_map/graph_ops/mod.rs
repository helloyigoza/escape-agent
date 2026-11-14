//! Structural graph and multi-hop operations (`pack_task`, `communities`,
//! `symbol`, `expand`, `impact`, `references`, `connect`, `affected`, `trace`,
//! `batch`, `lego`, `slice`, `cochange`, `doc_drift`, `flags`, `quality`)
//! backed by the persistent `ripwire --mcp` pool with CLI and cached pure-Rust
//! native fallbacks plus SCIP / `oc-lsp` precision overlays.

mod exec_batch;
mod exec_deep;
mod exec_query;
mod mcp;

pub(in crate::code_map) use self::exec_batch::extract_xml_attr;
pub(crate) use self::mcp::parse_mcp_json_text;
pub(in crate::code_map) use self::mcp::{
    McpAttempt, add_fallback_metadata, run_mcp_mode, timeout_error,
};

#[cfg(test)]
mod tests {
    use super::exec_batch::{parse_batch_item, parse_connect_endpoints};

    #[test]
    fn parses_connect_endpoints_and_batch_items() {
        assert_eq!(
            parse_connect_endpoints("execute_map -> run_ripwire"),
            Some(("execute_map".to_string(), "run_ripwire".to_string()))
        );
        assert_eq!(
            parse_connect_endpoints("Alpha..Beta"),
            Some(("Alpha".to_string(), "Beta".to_string()))
        );
        assert_eq!(
            parse_connect_endpoints("Alpha Beta"),
            Some(("Alpha".to_string(), "Beta".to_string()))
        );
        assert_eq!(parse_connect_endpoints("only_one"), None);

        let item = parse_batch_item("find_symbol:CodeMapTool").unwrap();
        assert_eq!(item.verb, "find_symbol");
        assert_eq!(item.arg, "CodeMapTool");
        assert!(item.supports_mcp_batch());
    }
}
