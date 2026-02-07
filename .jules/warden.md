# Warden's Journal 🔒

**2025-05-15 - Hardening Vector Index & WAL against DoS**
**Threat:**
1. `HnswIndexBuilder` allowed arbitrary `dimensions` and `capacity`, enabling OOM attacks via configuration (e.g. 100M capacity).
2. `HnswIndex::load_mappings` read entire files into memory without size checks, enabling OOM via sparse files.
3. `ConcurrentWal` allowed arbitrary `num_stripes` and `stripe_capacity`, enabling OOM attacks via configuration (e.g. 100M capacity).

**Defense:**
1. Enforced `MAX_VECTOR_DIMENSIONS` (100,000) and `MAX_HNSW_CAPACITY` (10,000,000) in `HnswIndexBuilder` and `HnswIndex::load`.
2. Enforced `MAX_MAPPINGS_FILE_SIZE` (2GB) in `load_mappings_with_integrity` using `File::metadata()` before reading.
3. Enforced `MAX_NUM_STRIPES` (1024) and `MAX_STRIPE_CAPACITY` (10,000,000) in `ConcurrentWal::new`.
