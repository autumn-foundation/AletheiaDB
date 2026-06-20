**Pre-allocating Vectors in Persistent Commit Log**
**Learning:** Pre-allocating `Vec` capacity based on file size metadata before calling `read_to_end()` eliminates dynamic reallocation overhead during the read loop. Additionally, heuristically estimating the number of parsed entries using the file buffer size and an average entry length provides an excellent capacity hint for parsing vectors.
**Action:** Always check `file.metadata()` to derive file lengths before buffering I/O into a vector, and propagate those capacity hints to dependent allocations where feasible.
