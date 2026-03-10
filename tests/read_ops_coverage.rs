use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::{ReadOps, WriteOps};
use aletheiadb::core::error::Result;
use aletheiadb::core::graph::Edge;
use aletheiadb::core::id::{EdgeId, NodeId};

struct DefaultReadOpsWrapper {
    tx: aletheiadb::api::transaction::ReadTransaction,
}

impl ReadOps for DefaultReadOpsWrapper {
    fn get_node(&self, id: NodeId) -> Result<aletheiadb::core::graph::Node> {
        self.tx.get_node(id)
    }

    fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        self.tx.get_edge(id)
    }

    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.tx.get_outgoing_edges(node_id)
    }

    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.tx.get_incoming_edges(node_id)
    }

    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.tx.get_outgoing_edges_with_label(node_id, label)
    }

    fn node_count(&self) -> usize {
        self.tx.node_count()
    }

    fn edge_count(&self) -> usize {
        self.tx.edge_count()
    }

    fn find_nodes_by_property(
        &self,
        label: &str,
        property_key: &str,
        property_value: &aletheiadb::core::property::PropertyValue,
    ) -> Vec<NodeId> {
        self.tx
            .find_nodes_by_property(label, property_key, property_value)
    }
}

#[test]
fn test_read_ops_default_get_edges() {}
#[test]
fn test_get_edges_historical_coverage() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .write(|tx| tx.create_node("Node", Default::default()))
        .unwrap();
    let n2 = db
        .write(|tx| tx.create_node("Node", Default::default()))
        .unwrap();
    let e1 = db
        .write(|tx| tx.create_edge(n1, n2, "L", Default::default()))
        .unwrap();

    let tx = db.read_transaction().unwrap();
    let wrapper = DefaultReadOpsWrapper { tx };

    let edges = wrapper.get_edges(&[e1, EdgeId::new(99).unwrap()]).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].id, e1);
}
