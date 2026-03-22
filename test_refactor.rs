use serde::de::DeserializeOwned;
use std::fmt::Display;

fn parse_args<T: DeserializeOwned>(args: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {}", e))
}
