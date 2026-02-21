import sys

with open("src/index/vector/hnsw.rs", "r") as f:
    lines = f.readlines()

# Fix block at 1035
# Line 1035: self.retry_usearch(
# Line 1036: || index.reserve(new_capacity),
# Line 1037: "Failed to expand capacity (race recovery)",
# Line 1038: ) {
# Line 1039: return Err(e);
# Line 1040: }

start_1 = 1035 - 1 # 0-indexed
end_1 = 1040

# Verify content
if "self.retry_usearch(" in lines[start_1] and "race recovery" in lines[start_1+2]:
    lines[start_1] = '                        self.retry_usearch(\n'
    lines[start_1+1] = '                            || index.reserve(new_capacity),\n'
    lines[start_1+2] = '                            "Failed to expand capacity (race recovery)",\n'
    lines[start_1+3] = '                        )?;\n'
    lines[start_1+4] = '' # delete
    lines[start_1+5] = '' # delete

# Fix block at 1096
# Line 1096: self.retry_usearch(
# Line 1097: || index.remove(key),
# Line 1098: "Failed to rollback vector after concurrent add",
# Line 1099: ) {
# Line 1100: return Err(e);
# Line 1101: }

start_2 = 1096 - 1
end_2 = 1101

if "self.retry_usearch(" in lines[start_2] and "concurrent add" in lines[start_2+2]:
    lines[start_2] = '                        self.retry_usearch(\n'
    lines[start_2+1] = '                            || index.remove(key),\n'
    lines[start_2+2] = '                            "Failed to rollback vector after concurrent add",\n'
    lines[start_2+3] = '                        )?;\n'
    lines[start_2+4] = '' # delete
    lines[start_2+5] = '' # delete

with open("src/index/vector/hnsw.rs", "w") as f:
    f.writelines(lines)
