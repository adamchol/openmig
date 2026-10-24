//! Deterministic dependency ordering for plan steps.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};

/// Sort priority among steps whose dependencies are satisfied: lower sorts first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Priority {
    pub phase: u8,
    pub rank: u16,
    pub key: String,
}

impl Priority {
    pub fn new(phase: u8, rank: u16, key: impl Into<String>) -> Self {
        Self {
            phase,
            rank,
            key: key.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(usize);

/// A graph of items with "must come before" edges, sorted with Kahn's algorithm. Among ready
/// items the smallest [`Priority`] wins, then insertion order, so output is deterministic.
#[derive(Debug)]
pub struct DepGraph<T> {
    items: Vec<T>,
    priority: Vec<Priority>,
    successors: Vec<BTreeSet<usize>>,
}

/// The graph has a cycle; `items` are the members that could not be ordered.
#[derive(Debug)]
pub struct CycleError<T> {
    pub items: Vec<T>,
}

impl<T> Default for DepGraph<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            priority: Vec::new(),
            successors: Vec::new(),
        }
    }
}

impl<T> DepGraph<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn add(&mut self, item: T, priority: Priority) -> NodeId {
        self.items.push(item);
        self.priority.push(priority);
        self.successors.push(BTreeSet::new());
        NodeId(self.items.len() - 1)
    }

    pub fn get(&self, id: NodeId) -> &T {
        &self.items[id.0]
    }

    pub fn ids(&self) -> impl Iterator<Item = NodeId> + use<T> {
        (0..self.items.len()).map(NodeId)
    }

    /// `before` must be ordered before `after`.
    pub fn edge(&mut self, before: NodeId, after: NodeId) {
        if before != after {
            self.successors[before.0].insert(after.0);
        }
    }

    /// Like [`DepGraph::edge`] but dropped when it would create a cycle. Returns whether the edge
    /// was kept. Used for dependencies guessed from SQL text.
    pub fn soft_edge(&mut self, before: NodeId, after: NodeId) -> bool {
        if before == after || self.reaches(after.0, before.0) {
            return false;
        }
        self.successors[before.0].insert(after.0);
        true
    }

    fn reaches(&self, from: usize, to: usize) -> bool {
        let mut seen = vec![false; self.items.len()];
        let mut stack = vec![from];
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if std::mem::replace(&mut seen[n], true) {
                continue;
            }
            stack.extend(self.successors[n].iter().copied());
        }
        false
    }

    pub fn sort(self) -> Result<Vec<T>, CycleError<T>> {
        let n = self.items.len();
        let mut indegree = vec![0usize; n];
        for succ in &self.successors {
            for &s in succ {
                indegree[s] += 1;
            }
        }
        let mut ready: BinaryHeap<Reverse<(Priority, usize)>> = (0..n)
            .filter(|&i| indegree[i] == 0)
            .map(|i| Reverse((self.priority[i].clone(), i)))
            .collect();
        let mut order = Vec::with_capacity(n);
        while let Some(Reverse((_, i))) = ready.pop() {
            order.push(i);
            for &s in &self.successors[i] {
                indegree[s] -= 1;
                if indegree[s] == 0 {
                    ready.push(Reverse((self.priority[s].clone(), s)));
                }
            }
        }
        let mut slots: Vec<Option<T>> = self.items.into_iter().map(Some).collect();
        if order.len() != n {
            let items = (0..n)
                .filter(|&i| indegree[i] > 0)
                .filter_map(|i| slots[i].take())
                .collect();
            return Err(CycleError { items });
        }
        Ok(order.into_iter().filter_map(|i| slots[i].take()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_then_edges() {
        let mut g = DepGraph::new();
        let view = g.add("view", Priority::new(0, 30, "v"));
        let table = g.add("table", Priority::new(0, 10, "t"));
        let func = g.add("function", Priority::new(0, 40, "f"));
        g.edge(func, view);
        g.edge(table, view);
        assert_eq!(g.sort().unwrap(), vec!["table", "function", "view"]);
    }

    #[test]
    fn soft_edges_never_create_cycles() {
        let mut g = DepGraph::new();
        let a = g.add("a", Priority::default());
        let b = g.add("b", Priority::default());
        g.edge(a, b);
        assert!(!g.soft_edge(b, a));
        assert_eq!(g.sort().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn cycles_are_reported() {
        let mut g = DepGraph::new();
        let a = g.add("a", Priority::default());
        let b = g.add("b", Priority::default());
        let c = g.add("c", Priority::default());
        g.edge(a, b);
        g.edge(b, a);
        let _ = c;
        let err = g.sort().unwrap_err();
        assert_eq!(err.items, vec!["a", "b"]);
    }
}
