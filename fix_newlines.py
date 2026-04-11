import re

files = [
    "src/storage/sharding/coordinator.rs",
    "src/storage/sharding/rebalance.rs",
    "src/query/executor/mod.rs",
    "src/storage/index_persistence/temporal.rs",
]

for path in files:
    with open(path, "r") as f:
        content = f.read()

    lines = content.split('\n')
    new_lines = []

    for i, line in enumerate(lines):
        if line.strip() == "":
            if i > 0 and i < len(lines) - 1:
                prev_line = lines[i-1].strip()
                if prev_line.startswith("///"):
                    # skip this empty line
                    continue
        new_lines.append(line)

    with open(path, "w") as f:
        f.write("\n".join(new_lines))
