import re

with open("Cargo.toml", "r") as f:
    content = f.read()

content = re.sub(r'ort = \{ version = "2.0.0-rc.0", optional = true \}\n', '', content)
content = re.sub(r'tokenizers = \{ version = "0.22", optional = true \}\n', '', content)
content = re.sub(r'embedding-onnx = \["embeddings", "dep:ort", "dep:tokenizers", "dep:num_cpus"\]\n', '', content)
content = re.sub(r'    "embedding-onnx",\n', '', content)

# Remove the embedding_onnx example completely
example_block = r'''\[\[example\]\]
name = "embedding_onnx"
required-features = \["embedding-onnx"\]'''
content = re.sub(example_block, '', content)

with open("Cargo.toml", "w") as f:
    f.write(content)
