use std::collections::{BTreeMap, BTreeSet};

use coder_config::{WorkflowEdgeSpec, WorkflowNodeSpec, WorkflowSpec};

use crate::{workflow_control::WorkflowSignal, WorkflowError};

#[derive(Debug)]
pub(crate) struct WorkflowGraph<'a> {
    pub(crate) start_node_id: String,
    nodes: BTreeMap<&'a str, &'a WorkflowNodeSpec>,
    edges: Vec<&'a WorkflowEdgeSpec>,
}

impl<'a> WorkflowGraph<'a> {
    pub(crate) fn new(workflow: &'a WorkflowSpec) -> Result<Self, WorkflowError> {
        let start_node_id = workflow
            .nodes
            .first()
            .map(|node| node.id.clone())
            .ok_or_else(|| {
                WorkflowError::InvalidConfig("workflow_start_node_missing".to_owned())
            })?;
        let mut seen = BTreeSet::new();
        let mut nodes = BTreeMap::new();
        for node in &workflow.nodes {
            if !seen.insert(node.id.as_str()) {
                return Err(WorkflowError::InvalidConfig(format!(
                    "duplicate workflow node '{}'",
                    node.id
                )));
            }
            nodes.insert(node.id.as_str(), node);
        }
        for edge in &workflow.edges {
            if !nodes.contains_key(edge.from.as_str()) {
                return Err(WorkflowError::InvalidConfig(format!(
                    "workflow edge source '{}' does not exist",
                    edge.from
                )));
            }
            if !nodes.contains_key(edge.to.as_str()) {
                return Err(WorkflowError::InvalidConfig(format!(
                    "workflow edge target '{}' does not exist",
                    edge.to
                )));
            }
        }
        Ok(Self {
            start_node_id,
            nodes,
            edges: workflow.edges.iter().collect(),
        })
    }

    pub(crate) fn node(&self, node_id: &str) -> Result<&'a WorkflowNodeSpec, WorkflowError> {
        self.nodes.get(node_id).copied().ok_or_else(|| {
            WorkflowError::InvalidConfig(format!("workflow node '{node_id}' does not exist"))
        })
    }

    pub(crate) fn select_edge(
        &self,
        node_id: &str,
        signal: WorkflowSignal,
    ) -> Option<&'a WorkflowEdgeSpec> {
        self.edges
            .iter()
            .copied()
            .find(|edge| edge.from == node_id && edge.on == signal.as_str())
    }
}

pub(crate) fn should_repair_with_executor(
    graph: &WorkflowGraph<'_>,
    next_node_id: &str,
    signal: WorkflowSignal,
) -> bool {
    matches!(
        signal,
        WorkflowSignal::Blocked | WorkflowSignal::Failed | WorkflowSignal::Continue
    ) && graph
        .node(next_node_id)
        .ok()
        .is_some_and(|node| node.agent == "executor" || node.id == "executor")
}

pub(crate) fn should_route_feedback_to_workflow_planner(
    graph: &WorkflowGraph<'_>,
    next_node_id: &str,
) -> bool {
    graph.node(next_node_id).ok().is_some_and(|node| {
        node.id == "planner" || node.agent == "workflow-planner" || node.agent == "planner"
    })
}
