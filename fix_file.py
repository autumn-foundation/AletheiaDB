with open("src/index/vector/distributed.rs", "r") as f:
    lines = f.readlines()

new_lines = []
for line in lines:
    new_lines.append(line)

# Wait, the problem is my test is outside the `mod tests {` block because I appended it at the end of the file, which is after `}` that closes `mod tests`.
# And `cargo fmt` fails or complains because of indentation or something else.
# Wait, the failure is `cargo fmt --all -- --check`.
