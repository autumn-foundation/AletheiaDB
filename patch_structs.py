import re

with open("src/index/vector/distributed.rs", "r") as f:
    content = f.read()

# Replace <C: VectorNodeClient> with nothing, and C with MockVectorNodeClient
content = content.replace("impl<C: VectorNodeClient> fmt::Debug for NodeConnection<C>", "impl fmt::Debug for NodeConnection")
content = content.replace("impl<C: VectorNodeClient> NodeConnection<C>", "impl NodeConnection")
content = content.replace("impl<C: VectorNodeClient> fmt::Debug for DistributedVectorIndex<C>", "impl fmt::Debug for DistributedVectorIndex")
content = content.replace("impl<C: VectorNodeClient + 'static> DistributedVectorIndex<C>", "impl DistributedVectorIndex")
content = content.replace("impl<C: VectorNodeClient + 'static> VectorIndex for DistributedVectorIndex<C>", "impl VectorIndex for DistributedVectorIndex")
content = content.replace("NodeConnection<C>", "NodeConnection")
content = content.replace("DistributedVectorIndex<C>", "DistributedVectorIndex")
content = content.replace("struct NodeConnection<C: VectorNodeClient>", "struct NodeConnection")
content = content.replace("struct DistributedVectorIndex<C: VectorNodeClient>", "struct DistributedVectorIndex")
content = content.replace("<C: VectorNodeClient>", "")
content = content.replace("fn execute<T, F>(&self, f: F) -> Result<T>\n    where\n        F: FnOnce(&C) -> Result<T>,", "fn execute<T, F>(&self, f: F) -> Result<T>\n    where\n        F: FnOnce(&MockVectorNodeClient) -> Result<T>,")
content = content.replace("Arc<C>", "Arc<MockVectorNodeClient>")
content = content.replace("client: Arc<C>,", "client: Arc<MockVectorNodeClient>,")
content = content.replace("fn execute<T, F>(&self, f: F) -> Result<T>\n    where\n        F: FnOnce(&MockVectorNodeClient) -> Result<T>,\n    {\n", "fn execute<T, F>(&self, f: F) -> Result<T>\n    where\n        F: FnOnce(&MockVectorNodeClient) -> Result<T>,\n    {\n")


content = content.replace("/// `DistributedVectorIndex`] logic.", "/// `DistributedVectorIndex` logic.")
content = content.replace("/// [`VectorNodeClient`]", "/// `MockVectorNodeClient`")
content = content.replace("impl VectorNodeClient for MockVectorNodeClient {", "impl MockVectorNodeClient {")

content = content.replace("clients: Vec<Arc<C>>", "clients: Vec<Arc<MockVectorNodeClient>>")

# Remove VectorNodeClient from exports in src/index/vector/mod.rs
with open("src/index/vector/mod.rs", "r") as modf:
    modc = modf.read()
modc = modc.replace("VectorNodeClient, ", "")
with open("src/index/vector/mod.rs", "w") as modf:
    modf.write(modc)

with open("src/index/vector/distributed.rs", "w") as f:
    f.write(content)
