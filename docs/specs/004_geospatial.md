# 🔭 Vantage Spec: GeoSpatial Support (The "Where" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-004 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/index/spatial.rs` (Proposed) |

## 1. 👤 User Stories

> **As a** Logistics Manager or Smart City Planner,
> **I want to** filter and find graph nodes based on their physical geographic location (Lat/Lon) and proximity to other points,
> **So that** I can answer questions like "Which sensors within 5km of Downtown reported a failure yesterday?" without exporting data to an external GIS system.

> **As a** Real Estate AI Agent,
> **I want to** find properties similar to "Luxury Villa" (Vector) located within 2km of "Central Park" (Geo),
> **So that** I can recommend relevant listings that match both the aesthetic and location requirements of the client.

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB currently masters:
-   **Who/Relational**: Graph (`MATCH (a)-[:KNOWS]->(b)`)
-   **When**: Temporal (`AS OF 2023`)
-   **What/Semantic**: Vector (`SIMILAR TO "concept"`)

**The Gap:**
It misses **Where**. Real-world data almost always has a spatial component.
Currently, users must query AletheiaDB for IDs, then query PostGIS/Elasticsearch for location, and join in the application. This is:
1.  **Slow** (network roundtrips, massive data transfer).
2.  **Complex** (managing two consistency domains).
3.  **Incomplete** (cannot easily do "3-hop graph traversal *constrained* by spatial bounds").

**ROI:**
-   **Completeness**: completing the "Who, What, Where, When" quadrant makes AletheiaDB a true "Universal Knowledge Engine".
-   **Performance**: Filtering by location *before* traversing or vector ranking massively prunes the search space.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1.  **Geo Primitives**:
    -   Support Point (Latitude, Longitude) as a first-class property type.
    -   Support Polygon (List of outer and inner rings of Points) for boundaries, compatible with GeoJSON `Polygon` and `MultiPolygon` types.
    -   **Standard Formats**: Must support ingestion and output in WKT (Well-Known Text) and GeoJSON formats.

2.  **Spatial Indexing**:
    -   Must implement a spatial index efficient for range and k-NN queries (e.g., R-Tree or Quadtree).
    -   Index must be persistent and support standard CRUD operations.
    -   **Hybrid Indexing**: Must support combined Spatial + Vector + Temporal queries. The query planner should be able to optimize "Near X AND Similar to Y" by choosing the most selective index first.

3.  **Query API**:
    -   `WITHIN_DISTANCE(point, distance_meters)`: Circle search.
    -   `WITHIN_POLYGON(polygon)`: Boundary search.
    -   `NEAREST(point, k)`: k-NN for spatial distance from a given point.

4.  **Composition (The "Killer Feature")**:
    -   Must seamlessly integrate with Graph and Vector queries.
    -   *Example*: "Find Suppliers (Graph) located within 50km of Factory X (Geo) providing 'Electronic Components' (Vector)."

### Non-Functional Requirements
-   **Accuracy**: Must use Geodetic distance (Haversine/Vincenty), not just Euclidean (so it works globally).
-   **Scale**: Sub-10ms queries for "Find points in radius" on a 10M point dataset, for queries returning up to 1,000 points.

## 4. 🚫 Out of Scope (Phase 1)

-   **Complex Projections**: Only WGS84 (EPSG:4326) will be supported initially. No NAD83, Web Mercator conversions.
-   **Raster Data**: No satellite imagery or heatmaps. Vector data only.
-   **Routing Engine**: We provide the graph and the coordinates; we do not provide a "Turn-by-turn navigation" solver (Use Valhalla/OSRM for that).

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Types** | Primitives only | Need specialized Geo Point type | Add new Property Type |
| **Index** | Hash, Vector (HNSW) | Spatial Indexing | Implement spatial index |
| **Query** | Exact match / Similarity | Spatial Proximity | Extend Query capabilities |
| **Filter** | Exact match / Range | Spatial Predicates | Add Spatial predicates |

## 6. 📅 Execution Plan

1.  **Data Types**: Add Geo Point and Polygon support to the type system.
2.  **Storage**: Implement persistent spatial indexing infrastructure (R-Tree recommended).
3.  **Integration**: Ensure node creation/updates automatically update the spatial index.
4.  **Query**: Add user-facing methods for distance and boundary checks.
5.  **Test**: Add benchmarks demonstrating Graph + Geo + Vector performance.
