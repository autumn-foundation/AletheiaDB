#!/bin/bash
cat src/mcp/server.rs | grep -c "match serde_json::from_value"
