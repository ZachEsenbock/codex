use crate::error::TaskError;
use crate::schema::TaskFile;
use crate::schema::TaskSpec;
use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use petgraph::visit::Topo;
use petgraph::Direction;
use std::collections::BTreeMap;

pub struct Dag {
    pub graph: DiGraph<String, ()>,
    pub index_by_id: BTreeMap<String, NodeIndex>,
}

impl Dag {
    pub fn build(tf: &TaskFile) -> Result<Self, TaskError> {
        // map id -> node
        let mut graph: DiGraph<String, ()> = DiGraph::new();
        let mut index_by_id: BTreeMap<String, NodeIndex> = BTreeMap::new();

        for t in &tf.tasks {
            if index_by_id.contains_key(&t.id) {
                return Err(TaskError::DuplicateTask(t.id.clone()));
            }
            let idx = graph.add_node(t.id.clone());
            index_by_id.insert(t.id.clone(), idx);
        }

        // edges
        for t in &tf.tasks {
            for dep in &t.depends_on {
                let from = match index_by_id.get(dep) {
                    Some(i) => *i,
                    None => return Err(TaskError::MissingDependency(dep.clone())),
                };
                let to = match index_by_id.get(&t.id) {
                    Some(i) => *i,
                    None => return Err(TaskError::MissingDependency(t.id.clone())),
                };
                graph.add_edge(from, to, ());
            }
        }

        // cycle check
        if has_cycle(&graph) {
            return Err(TaskError::CyclicDag);
        }

        Ok(Self { graph, index_by_id })
    }

    /// Return execution waves (levels) in topological order.
    pub fn levels(&self) -> Vec<Vec<String>> {
        let mut indeg: BTreeMap<NodeIndex, usize> = self
            .graph
            .node_indices()
            .map(|i| {
                (
                    i,
                    self.graph
                        .neighbors_directed(i, Direction::Incoming)
                        .count(),
                )
            })
            .collect();

        let mut ready: Vec<NodeIndex> = indeg
            .iter()
            .filter_map(|(i, d)| if *d == 0 { Some(*i) } else { None })
            .collect();

        // ensure deterministic order of nodes within a level by sorting by task id
        ready.sort_by_key(|i| self.graph[*i].clone());

        let mut levels: Vec<Vec<String>> = Vec::new();

        while !ready.is_empty() {
            let wave = ready.clone();
            ready.clear();
            let mut level_ids = Vec::new();
            for n in wave {
                level_ids.push(self.graph[n].clone());
                for succ in self.graph.neighbors_directed(n, Direction::Outgoing) {
                    if let Some(e) = indeg.get_mut(&succ) {
                        *e -= 1;
                        if *e == 0 {
                            ready.push(succ);
                        }
                    }
                }
            }
            // sort this level's ids for deterministic output
            level_ids.sort();
            levels.push(level_ids);
        }
        levels
    }

    pub fn task_by_id<'a>(&self, tf: &'a TaskFile, id: &str) -> Option<&'a TaskSpec> {
        tf.tasks.iter().find(|t| t.id == id)
    }
}

fn has_cycle(graph: &DiGraph<String, ()>) -> bool {
    let mut topo = Topo::new(graph);
    let mut count = 0usize;
    while topo.next(graph).is_some() {
        count += 1;
    }
    count != graph.node_count()
}
