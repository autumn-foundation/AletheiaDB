import re

with open('src/storage/index_persistence/graph.rs', 'r') as f:
    content = f.read()

content = content.replace('    let mmap = unsafe { Mmap::map(&file)? };', '    // SAFETY: We only read from the memory map, never write. The file is opened read-only.\n    // The mapping is valid for the lifetime of this function and is automatically unmapped\n    // when dropped. We have verified the file size above to prevent out-of-bounds reads.\n    let mmap = unsafe { Mmap::map(&file)? };')

with open('src/storage/index_persistence/graph.rs', 'w') as f:
    f.write(content)
