with open("/app/agents/feature-catalyst/src/experimental/catalyst.rs", "r") as f:
    content = f.read()

content = content.replace("use crate::core::vector::ops;\nuse crate::api::transaction::ReadOps;", "use crate::api::transaction::ReadOps;\nuse crate::core::vector::ops;")

with open("/app/agents/feature-catalyst/src/experimental/catalyst.rs", "w") as f:
    f.write(content)
