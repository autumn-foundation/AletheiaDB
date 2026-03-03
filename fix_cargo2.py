import re

with open("Cargo.toml", "r") as f:
    content = f.read()

content = re.sub(r'ort = \{ version = "2.0.0-rc.0", optional = true \}\n', '', content)
content = re.sub(r'tokenizers = \{ version = "0.22", optional = true \}\n', '', content)
content = re.sub(r'embedding-onnx = \["embeddings", "dep:ort", "dep:tokenizers", "dep:num_cpus"\]\n', '', content)
content = re.sub(r'    "embedding-onnx",\n', '', content)

# Remove the embedding_onnx example completely
content = re.sub(r'\[\[example\]\]\nname = "embedding_onnx"\nrequired-features = \["embedding-onnx"\]\n', '', content)

with open("Cargo.toml", "w") as f:
    f.write(content)
