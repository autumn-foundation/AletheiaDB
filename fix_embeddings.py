import re

with open("src/embeddings/mod.rs", "r") as f:
    content = f.read()

content = re.sub(r'//! - `embedding-onnx` - Local ONNX models\n', '', content)
content = re.sub(r'    feature = "embedding-onnx",\n', '', content)

with open("src/embeddings/mod.rs", "w") as f:
    f.write(content)

with open("src/embeddings/providers/mod.rs", "r") as f:
    content = f.read()

content = re.sub(r'#\[cfg\(feature = "embedding-onnx"\)\]\npub mod onnx;\n', '', content)

with open("src/embeddings/providers/mod.rs", "w") as f:
    f.write(content)
