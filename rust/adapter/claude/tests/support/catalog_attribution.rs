use crate::native::McpServer;

use super::*;

#[test]
fn refresh_preserves_attribution_from_the_last_scoped_list() {
    let mut catalog = Catalog::default();
    let native = "Bash";
    let mcp = "mcp__files__read";
    let init = InitFrame {
        tools: Some(vec![native.into(), mcp.into()]),
        mcp_servers: vec![McpServer {
            name: "files".into(),
            ..McpServer::default()
        }],
        ..InitFrame::default()
    };
    catalog.list(Some("session".into()));
    catalog.observe(&init);
    catalog.list(None);
    assert_eq!(catalog.attribution[native], NATIVE_SOURCE);
    assert!(!catalog.attribution.contains_key(mcp));

    catalog.observe(&InitFrame::default());
    assert!(catalog.attribution.is_empty());
    catalog.observe(&init);

    catalog.list(Some("session".into()));
    assert_eq!(catalog.attribution[mcp], "mcp:files");
    catalog.observe(&InitFrame {
        tools: Some(vec![mcp.into()]),
        ..InitFrame::default()
    });
    assert_eq!(catalog.attribution[native], NATIVE_SOURCE);
    assert_eq!(catalog.attribution[mcp], "mcp:files");

    catalog.list(Some("session".into()));
    assert!(!catalog.attribution.contains_key(native));
    assert_eq!(catalog.attribution[mcp], NATIVE_SOURCE);
}
