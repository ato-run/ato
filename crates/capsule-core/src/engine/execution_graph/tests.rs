use super::{
    ExecutionGraphBuildInput, ExecutionGraphBuilder, ExecutionGraphNode, GraphDependencyInput,
    GraphSourceInput, GraphTargetInput,
};

fn sample_input() -> ExecutionGraphBuildInput {
    ExecutionGraphBuildInput {
        source: Some(GraphSourceInput {
            identifier: "src://workspace".into(),
        }),
        targets: vec![
            GraphTargetInput {
                identifier: "entry://main".into(),
                runtime: "runtime://node".into(),
            },
            GraphTargetInput {
                identifier: "entry://worker".into(),
                runtime: "runtime://node".into(),
            },
        ],
        dependencies: vec![
            GraphDependencyInput {
                provider: "provider://npm".into(),
                output: "output://lodash".into(),
            },
            GraphDependencyInput {
                provider: "provider://cargo".into(),
                output: "output://serde".into(),
            },
        ],
        host: None,
        policy: None,
    }
}

#[test]
fn nodes_are_emitted_in_deterministic_order_regardless_of_input_order() {
    let forward = sample_input();
    let mut reversed = sample_input();
    reversed.targets.reverse();
    reversed.dependencies.reverse();

    let g_forward = ExecutionGraphBuilder::build(forward);
    let g_reversed = ExecutionGraphBuilder::build(reversed);

    assert_eq!(g_forward.nodes, g_reversed.nodes);
}

#[test]
fn edges_are_emitted_in_deterministic_order_regardless_of_input_order() {
    let forward = sample_input();
    let mut reversed = sample_input();
    reversed.targets.reverse();
    reversed.dependencies.reverse();

    let g_forward = ExecutionGraphBuilder::build(forward);
    let g_reversed = ExecutionGraphBuilder::build(reversed);

    assert_eq!(g_forward.edges, g_reversed.edges);
}

#[test]
fn each_dependency_input_emits_a_provider_node_in_stable_order() {
    let input = ExecutionGraphBuildInput {
        source: None,
        targets: Vec::new(),
        dependencies: vec![
            GraphDependencyInput {
                provider: "provider://zeta".into(),
                output: "output://zeta-out".into(),
            },
            GraphDependencyInput {
                provider: "provider://alpha".into(),
                output: "output://alpha-out".into(),
            },
        ],
        host: None,
        policy: None,
    };

    let graph = ExecutionGraphBuilder::build(input);

    let providers: Vec<&str> = graph
        .nodes
        .iter()
        .filter_map(|node| match node {
            ExecutionGraphNode::Provider { identifier } => Some(identifier.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(providers, vec!["provider://alpha", "provider://zeta"]);
}
