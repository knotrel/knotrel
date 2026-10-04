//! Caller-owned reusable BFS storage; no graph answers or IDs are retained.
use crate::{Graph, GraphError, NodeId};

/// Reusable temporary storage for [`Graph::connected_with_workspace`].
///
/// Each caller or worker owns its workspace. Separate workspaces permit shared
/// graph queries without internal locks. A workspace can be reused across graph
/// growth, mutations and different graphs. It never caches connectivity answers.
/// Marks use four bytes per registered vertex. Storage retains O(N + R) capacity for the largest registered node count N and
/// largest visited set R so far; drop the workspace to release that memory.
#[derive(Debug, Default)]
pub struct BfsWorkspace {
    visited: Vec<u32>,
    generation: u32,
    queue: Vec<usize>,
}

impl Graph {
    /// Tests connectivity using reusable caller-owned traversal storage.
    ///
    /// Unlike [`Graph::connected`], repeated calls reuse allocations. A generation
    /// counter makes old marks inactive without clearing them after each query.
    /// Traversal takes O(V_reached + E_examined), plus O(log V) endpoint lookup
    /// and amortized storage growth (initial growth is O(V)). Generation rollover
    /// clears the retained mark array once per u32::MAX non-reflexive queries,
    /// costing O(N) for the largest graph previously queried with this workspace.
    /// No answers are cached; queries always observe current edges.
    ///
    /// ```
    /// use knotrel_core::{BfsWorkspace, Graph};
    /// let mut graph = Graph::new();
    /// graph.link(10, 20)?;
    /// let mut workspace = BfsWorkspace::default();
    /// assert!(graph.connected_with_workspace(10, 20, &mut workspace)?);
    /// graph.cut(10, 20)?;
    /// assert!(!graph.connected_with_workspace(10, 20, &mut workspace)?);
    /// # Ok::<(), knotrel_core::GraphError>(())
    /// ```
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownNode`] for absent endpoints, source first.
    pub fn connected_with_workspace(
        &self,
        source: NodeId,
        target: NodeId,
        workspace: &mut BfsWorkspace,
    ) -> Result<bool, GraphError> {
        let a = *self
            .ids
            .get(&source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let b = *self
            .ids
            .get(&target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        if a == b {
            return Ok(true);
        }
        if workspace.visited.len() < self.adjacency.len() {
            workspace.visited.resize(self.adjacency.len(), 0);
        }
        workspace.generation = workspace.generation.wrapping_add(1);
        if workspace.generation == 0 {
            workspace.visited.fill(0);
            workspace.generation = 1;
        }
        let generation = workspace.generation;
        workspace.visited[a] = generation;
        workspace.queue.push(a);
        let mut cursor = 0;
        let mut found = false;
        // Mark on enqueue. The generation distinguishes every call, including
        // queries on another graph with unrelated indices or after early exit.
        'search: while cursor < workspace.queue.len() {
            for &neighbor in &self.adjacency[workspace.queue[cursor]] {
                if neighbor == b {
                    found = true;
                    break 'search;
                }
                if workspace.visited[neighbor] != generation {
                    workspace.visited[neighbor] = generation;
                    workspace.queue.push(neighbor);
                }
            }
            cursor += 1;
        }
        workspace.queue.clear();
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_rollover_clears_old_marks() {
        let mut graph = Graph::new();
        graph.link(1, 2).unwrap();
        graph.link(2, 3).unwrap();
        let mut workspace = BfsWorkspace::default();
        assert!(
            graph
                .connected_with_workspace(1, 2, &mut workspace)
                .unwrap()
        );
        workspace.generation = u32::MAX;
        workspace.visited.fill(1);
        assert!(
            graph
                .connected_with_workspace(1, 3, &mut workspace)
                .unwrap()
        );
        assert_eq!(workspace.generation, 1);
    }
}
