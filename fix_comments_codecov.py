import re

with open('src/storage/sharding/coordinator.rs', 'r') as f:
    lines = f.readlines()

for i in range(len(lines)):
    line = lines[i]
    if line.strip().startswith("///"):
        # If it has some text (not just empty ///) and doesn't already have // at the end
        if len(line.strip()) > 3 and not line.strip().endswith("//"):
            # Append // at the end of the line
            # Ensure we maintain the trailing newline
            lines[i] = line.rstrip('\n') + " //\n"

with open('src/storage/sharding/coordinator.rs', 'w') as f:
    f.writelines(lines)
