//! The doc is the contract MCP clients plan calls from; name-level rot
//! becomes a CI failure. (Result-shape drift is a separate, future guard.)

use roughcut_core::tools;

#[test]
fn tool_api_doc_names_match_the_registry() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/tool-api.md");
    let doc = std::fs::read_to_string(&path).expect("read docs/tool-api.md");
    // Tool bullets look like "- `name { args } -> result`"; take the first
    // backticked token of each bullet line in the Tools section.
    let tools_section = doc
        .split("## Tools")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("## Tools section");
    let documented: std::collections::HashSet<String> = tools_section
        .lines()
        .filter(|l| l.trim_start().starts_with("- `"))
        .filter_map(|l| {
            let after = l.split('`').nth(1)?;
            Some(after.split_whitespace().next()?.to_string())
        })
        .collect();
    let registry: std::collections::HashSet<String> =
        tools::all_defs().into_iter().map(|d| d.name).collect();

    let undocumented: Vec<_> = registry.difference(&documented).collect();
    let ghosts: Vec<_> = documented.difference(&registry).collect();
    assert!(
        undocumented.is_empty(),
        "tools missing from docs/tool-api.md: {undocumented:?}"
    );
    assert!(ghosts.is_empty(), "docs/tool-api.md documents nonexistent tools: {ghosts:?}");
}
