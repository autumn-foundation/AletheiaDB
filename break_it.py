import sys

with open('src/storage/wal/group_commit.rs', 'r') as f:
    lines = f.readlines()

new_lines = []
skip = False
for line in lines:
    if "Check for flush errors specifically for this epoch" in line:
        skip = True
        new_lines.append(line) # Keep the comment
        new_lines.append("        // BROKEN FOR TESTING\n")
        continue

    if skip:
        if "Check for history eviction" in line:
            skip = False
            new_lines.append(line)
    else:
        new_lines.append(line)

with open('src/storage/wal/group_commit.rs', 'w') as f:
    f.writelines(new_lines)
