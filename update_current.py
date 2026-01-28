import sys

with open('src/storage/current.rs', 'r') as f:
    content = f.read()

# Update macro
old_macro = """/// Macro to generate direct methods that acquire a snapshot lock.
macro_rules! impl_direct_method {
    ($(#[$meta:meta])* pub fn $name:ident(&self, $($arg:ident: $type:ty),*) -> $ret:ty { $locked_name:ident }) => {
        $(#[$meta])*
        pub fn $name(&self, $($arg: $type),*) -> $ret {
            let _guard = self.snapshot_lock.read();
            self.$locked_name($($arg),*)
        }
    };
}"""

new_macro = """/// Macro to generate direct methods that acquire a snapshot lock.
///
/// This macro reduces boilerplate for methods that need to:
/// 1. Acquire a read lock on `snapshot_lock` (to coordinate with checkpoints)
/// 2. Call an internal `_locked` implementation
///
/// # Usage
///
/// ```ignore
/// impl_direct_method! {
///     /// Documentation for the public method
///     pub fn method_name(&self, arg1: Type1, arg2: Type2) -> Result<()> { internal_method_locked }
/// }
/// ```
macro_rules! impl_direct_method {
    ($(#[$meta:meta])* pub fn $name:ident(&self, $($arg:ident: $type:ty),*) -> $ret:ty { $locked_name:ident }) => {
        $(#[$meta])*
        pub fn $name(&self, $($arg: $type),*) -> $ret {
            let _snapshot_guard = self.snapshot_lock.read();
            self.$locked_name($($arg),*)
        }
    };
}"""

content = content.replace(old_macro, new_macro)

# Update guard naming in other methods (replace _guard with _snapshot_guard)
# This affects create_node, create_edge, delete_node, delete_edge which were not using the macro before
# but were manually updated in my thought process to use _snapshot_guard.
# Wait, in the file content I read, they still use `_guard`.
content = content.replace("let _guard = self.snapshot_lock.read();", "let _snapshot_guard = self.snapshot_lock.read();")

# Update direct methods to use macro and fix visibility
methods = [
    ("insert_node_direct", "insert_node_direct_locked", "node: Node, timestamp: Timestamp"),
    ("insert_edge_direct", "insert_edge_direct_locked", "edge: Edge"),
    ("update_node_direct", "update_node_direct_locked", "node: Node, timestamp: Timestamp"),
    ("update_edge_direct", "update_edge_direct_locked", "edge: Edge"),
    ("delete_node_direct", "delete_node_direct_locked", "id: NodeId, timestamp: Timestamp"),
    ("delete_edge_direct", "delete_edge_direct_locked", "id: EdgeId"),
]

# Specifically handle delete_node_direct signature which spans multiple lines in original
old_delete_node = """    /// Delete a node directly (used by WriteTransaction).
    pub fn delete_node_direct(&self, id: NodeId, timestamp: Timestamp) -> Result<()> {
        let _snapshot_guard = self.snapshot_lock.read();
        self.delete_node_direct_locked(id, timestamp)
    }

    /// Internal locked version of delete_node_direct.
    pub(crate) fn delete_node_direct_locked(
        &self,
        id: NodeId,
        timestamp: Timestamp,
    ) -> Result<()> {"""

new_delete_node = """    impl_direct_method! {
        /// Delete a node directly (used by WriteTransaction).
        pub fn delete_node_direct(&self, id: NodeId, timestamp: Timestamp) -> Result<()> { delete_node_direct_locked }
    }

    /// Internal locked version of delete_node_direct.
    #[doc(hidden)]
    pub fn delete_node_direct_locked(
        &self,
        id: NodeId,
        timestamp: Timestamp,
    ) -> Result<()> {"""

content = content.replace(old_delete_node, new_delete_node)

# Handle simple one-liners
for pub_name, locked_name, args in methods:
    if pub_name == "delete_node_direct": continue # Handled above

    # Construct old pattern to search
    # Note: we already replaced _guard with _snapshot_guard globally

    # Need to handle exact formatting including whitespace

    # insert_node_direct
    if pub_name == "insert_node_direct":
        old_pattern = """    /// Insert a node directly (used by WriteTransaction).
    /// Does not generate IDs - caller must provide them.
    pub fn insert_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> {
        let _snapshot_guard = self.snapshot_lock.read();
        self.insert_node_direct_locked(node, timestamp)
    }

    /// Internal locked version of insert_node_direct.
    pub(crate) fn insert_node_direct_locked(&self, node: Node, timestamp: Timestamp) -> Result<()> {"""

        new_pattern = """    impl_direct_method! {
        /// Insert a node directly (used by WriteTransaction).
        /// Does not generate IDs - caller must provide them.
        pub fn insert_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> { insert_node_direct_locked }
    }

    /// Internal locked version of insert_node_direct.
    #[doc(hidden)]
    pub fn insert_node_direct_locked(&self, node: Node, timestamp: Timestamp) -> Result<()> {"""
        content = content.replace(old_pattern, new_pattern)

    # insert_edge_direct
    elif pub_name == "insert_edge_direct":
        old_pattern = """    /// Insert an edge directly (used by WriteTransaction).
    /// Does not generate IDs or rebuild adjacency - caller must handle.
    pub fn insert_edge_direct(&self, edge: Edge) -> Result<()> {
        let _snapshot_guard = self.snapshot_lock.read();
        self.insert_edge_direct_locked(edge)
    }

    /// Internal locked version of insert_edge_direct.
    pub(crate) fn insert_edge_direct_locked(&self, edge: Edge) -> Result<()> {"""

        new_pattern = """    impl_direct_method! {
        /// Insert an edge directly (used by WriteTransaction).
        /// Does not generate IDs or rebuild adjacency - caller must handle.
        pub fn insert_edge_direct(&self, edge: Edge) -> Result<()> { insert_edge_direct_locked }
    }

    /// Internal locked version of insert_edge_direct.
    #[doc(hidden)]
    pub fn insert_edge_direct_locked(&self, edge: Edge) -> Result<()> {"""
        content = content.replace(old_pattern, new_pattern)

    # update_node_direct
    elif pub_name == "update_node_direct":
        old_pattern = """    /// Update a node directly (used by WriteTransaction).
    pub fn update_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> {
        let _snapshot_guard = self.snapshot_lock.read();
        self.update_node_direct_locked(node, timestamp)
    }

    /// Internal locked version of update_node_direct.
    pub(crate) fn update_node_direct_locked(&self, node: Node, timestamp: Timestamp) -> Result<()> {"""

        new_pattern = """    impl_direct_method! {
        /// Update a node directly (used by WriteTransaction).
        pub fn update_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> { update_node_direct_locked }
    }

    /// Internal locked version of update_node_direct.
    #[doc(hidden)]
    pub fn update_node_direct_locked(&self, node: Node, timestamp: Timestamp) -> Result<()> {"""
        content = content.replace(old_pattern, new_pattern)

    # update_edge_direct
    elif pub_name == "update_edge_direct":
        old_pattern = """    /// Update an edge directly (used by WriteTransaction).
    pub fn update_edge_direct(&self, edge: Edge) -> Result<()> {
        let _snapshot_guard = self.snapshot_lock.read();
        self.update_edge_direct_locked(edge)
    }

    /// Internal locked version of update_edge_direct.
    pub(crate) fn update_edge_direct_locked(&self, edge: Edge) -> Result<()> {"""

        new_pattern = """    impl_direct_method! {
        /// Update an edge directly (used by WriteTransaction).
        pub fn update_edge_direct(&self, edge: Edge) -> Result<()> { update_edge_direct_locked }
    }

    /// Internal locked version of update_edge_direct.
    #[doc(hidden)]
    pub fn update_edge_direct_locked(&self, edge: Edge) -> Result<()> {"""
        content = content.replace(old_pattern, new_pattern)

    # delete_edge_direct
    elif pub_name == "delete_edge_direct":
        old_pattern = """    /// Delete an edge directly (used by WriteTransaction).
    pub fn delete_edge_direct(&self, id: EdgeId) -> Result<()> {
        let _snapshot_guard = self.snapshot_lock.read();
        self.delete_edge_direct_locked(id)
    }

    /// Internal locked version of delete_edge_direct.
    pub(crate) fn delete_edge_direct_locked(&self, id: EdgeId) -> Result<()> {"""

        new_pattern = """    impl_direct_method! {
        /// Delete an edge directly (used by WriteTransaction).
        pub fn delete_edge_direct(&self, id: EdgeId) -> Result<()> { delete_edge_direct_locked }
    }

    /// Internal locked version of delete_edge_direct.
    #[doc(hidden)]
    pub fn delete_edge_direct_locked(&self, id: EdgeId) -> Result<()> {"""
        content = content.replace(old_pattern, new_pattern)

with open('src/storage/current.rs', 'w') as f:
    f.write(content)
