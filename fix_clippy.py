import sys

with open("src/index/vector/hnsw.rs", "r") as f:
    lines = f.readlines()

# Fix 1: line 1035 (approx)
# if let Err(e) = self.retry_usearch(
#     || index.reserve(new_capacity),
#     "Failed to expand capacity (race recovery)",
# ) {
#     return Err(e);
# }

for i in range(len(lines)):
    if "if let Err(e) = self.retry_usearch(" in lines[i]:
        # Check if next lines match the pattern
        if "|| index.reserve(new_capacity)," in lines[i+1]:
            # Replace with ?
            lines[i] = lines[i].replace("if let Err(e) = ", "")
            # Remove the { return Err(e); } part
            # It spans multiple lines.
            # line i: self.retry_usearch(
            # line i+1: || ...
            # line i+2: "..."
            # line i+3: ) {
            # line i+4: return Err(e);
            # line i+5: }

            # Simplified replacement logic:
            # Join the lines, replace the pattern, split again? No.
            pass

# Since exact line numbers might shift due to previous edits, I'll use simple search/replace logic based on context.

# Block 1
block1_start = -1
for i in range(len(lines)):
    if "if let Err(e) = self.retry_usearch(" in lines[i] and "index.reserve(new_capacity)" in lines[i+1]:
        block1_start = i
        break

if block1_start != -1:
    lines[block1_start] = lines[block1_start].replace("if let Err(e) = ", "")
    # line i+3 is `) {`
    lines[block1_start+3] = lines[block1_start+3].replace(") {", ")?;")
    # clear lines i+4 and i+5
    lines[block1_start+4] = ""
    lines[block1_start+5] = ""

# Block 2
block2_start = -1
for i in range(len(lines)):
    if "if let Err(e) = self.retry_usearch(" in lines[i] and "index.remove(key)" in lines[i+1]:
        block2_start = i
        break

if block2_start != -1:
    lines[block2_start] = lines[block2_start].replace("if let Err(e) = ", "")
    # line i+3 is `) {`
    lines[block2_start+3] = lines[block2_start+3].replace(") {", ")?;")
    # clear lines i+4 and i+5
    lines[block2_start+4] = ""
    lines[block2_start+5] = ""

with open("src/index/vector/hnsw.rs", "w") as f:
    f.writelines(lines)
