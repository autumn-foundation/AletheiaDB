import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Replace parse_timestamp matches
helper = """    fn parse_timestamp_arg(&self, timestamp_str: &str) -> Result<HybridTimestamp, CallToolResult> {
        self.parse_timestamp(timestamp_str).map_err(|e| self.error_json(&e))
    }

    fn parse_optional_tx_time_arg(&self, tx_time: Option<&str>) -> Result<Option<HybridTimestamp>, CallToolResult> {
        self.parse_optional_tx_time(tx_time).map_err(|e| self.error_json(&e))
    }"""
content = content.replace("""    fn parse_edge_id(&self, id: u64) -> Result<EdgeId, CallToolResult> {
        EdgeId::new(id).map_err(|e| self.error_json(&e.to_string()))
    }""", """    fn parse_edge_id(&self, id: u64) -> Result<EdgeId, CallToolResult> {
        EdgeId::new(id).map_err(|e| self.error_json(&e.to_string()))
    }\n\n""" + helper)

pattern_ts = re.compile(r"""        let (valid_time|tx_time) = match self\.parse_timestamp\(&(req\.valid_time|req\.transaction_time)\) \{\s*Ok\(t\) => t,\s*Err\(e\) => return self\.error_json\(&e\),\s*\};""")

def replace_match_ts(match):
    var_name = match.group(1)
    req_field = match.group(2)
    return f"""        let Ok({var_name}) = self.parse_timestamp_arg(&{req_field}) else {{
            return self.parse_timestamp_arg(&{req_field}).unwrap_err();
        }};"""

content = pattern_ts.sub(replace_match_ts, content)

pattern_opt_tx = re.compile(r"""        let tx_time = match self\.parse_optional_tx_time\(req\.transaction_time\.as_deref\(\)\) \{\s*Ok\(t\) => t,\s*Err\(e\) => return self\.error_json\(&e\),\s*\};""")

def replace_match_opt_tx(match):
    return f"""        let Ok(tx_time) = self.parse_optional_tx_time_arg(req.transaction_time.as_deref()) else {{
            return self.parse_optional_tx_time_arg(req.transaction_time.as_deref()).unwrap_err();
        }};"""

content = pattern_opt_tx.sub(replace_match_opt_tx, content)

with open("src/mcp/server.rs", "w") as f:
    f.write(content)

print(f"Replaced timestamp matches with guard clauses.")
