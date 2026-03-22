import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Fix the docs block issue which was accidentally replaced by adding at top
content = content.replace("use crate::core::hlc::HybridTimestamp;\n//! AletheiaDB MCP (Model Context Protocol) Server Integration", "//! AletheiaDB MCP (Model Context Protocol) Server Integration\nuse crate::core::hlc::HybridTimestamp;")

# Fix parse_optional_tx_time_arg
helper = """    fn parse_optional_tx_time_arg(
        &self,
        tx_time: Option<&str>,
    ) -> Result<HybridTimestamp, CallToolResult> {
        self.parse_optional_tx_time(tx_time)
            .map_err(|e| self.error_json(&e))
    }"""
content = content.replace("""    fn parse_optional_tx_time_arg(
        &self,
        tx_time: Option<&str>,
    ) -> Result<Option<HybridTimestamp>, CallToolResult> {
        self.parse_optional_tx_time(tx_time)
            .map_err(|e| self.error_json(&e))
    }""", helper)

with open("src/mcp/server.rs", "w") as f:
    f.write(content)

print(f"Fixed docs and signature.")
