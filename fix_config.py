import re

with open("src/config.rs", "r") as f:
    content = f.read()

# Replace fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::IoError(e.to_string()))?;
# with fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::IoError(format!("Failed to read config file '{}': {}", path.as_ref().display(), e)))?;

content = content.replace(
    "fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::IoError(e.to_string()))?;",
    "fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::IoError(format!(\"Failed to read config file '{}': {}\", path.as_ref().display(), e)))?;"
)

with open("src/config.rs", "w") as f:
    f.write(content)
