# 🔭 Vantage: Spec for Role-Based Access Control (RBAC)

## 👤 User Story
**As a** Database Administrator,
**I want to** define roles with specific permissions (e.g., Read-Only, Read-Write, Admin) and assign them to users,
**so that** I can restrict access to sensitive temporal graph data and ensure that only authorized personnel can modify the database or execute potentially destructive operations.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, anyone with network access to AletheiaDB has full administrative control over the entire database. This is a massive security vulnerability for enterprise deployments. Without Role-Based Access Control (RBAC), organizations cannot enforce the principle of least privilege, making the database unsuitable for multi-tenant environments, regulated industries (HIPAA, SOC2), or scenarios where different applications need varying levels of access to the same cluster. Implementing RBAC closes this critical enterprise gap, moving AletheiaDB from a "trusted network only" tool to a secure, enterprise-ready database.

**Success Metric Definition:**
- **Security:** Unauthorized read or write attempts are rejected with a 403 Forbidden status.
- **Performance:** Authorization checks add <1ms overhead to query execution time.
- **Auditability:** All authorization failures are logged with the user identity, attempted action, and target resource for security auditing.

**Gap Analysis:**
- Market alternatives like Neo4j (Enterprise Edition) and PostgreSQL have robust, built-in RBAC systems. AletheiaDB currently lacks any native authentication or authorization mechanisms, assuming deployment behind a trusted API gateway.

## ✅ Acceptance Criteria
- Must define a set of built-in roles: `admin` (full access), `writer` (read and modify data), and `reader` (read-only access to nodes, edges, and vectors).
- Must provide an API (or Cypher commands) to create users, assign roles to users, and manage credentials.
- Must intercept all incoming queries (HTTP and Cypher) and validate the user's role against the required permissions for the operation before execution.
- Must support passing authentication credentials (e.g., Bearer tokens or Basic Auth) via standard HTTP headers.

## 🚫 Out of Scope
- Integration with external identity providers (LDAP, Active Directory, OAuth/OIDC). Phase 1 focuses entirely on native, internal RBAC.
- Row-level or property-level security (e.g., User A can only read nodes where `department = 'HR'`). Phase 1 provides database-level coarse-grained permissions.
