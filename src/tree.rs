use std::collections::{HashMap, HashSet};

use serde::Serialize;

/// A row from the existing `WorkflowNodeRow` — we accept a simplified trait-like interface.
pub struct TreeNode {
    pub name: String,
    pub node_type: String,
    pub credentials: Vec<String>,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TreeData {
    pub roots: Vec<String>,
    pub edges: Vec<TreeEdge>,
    pub unconnected: Vec<String>,
}

/// Build adjacency list from connection rows.
/// Each entry: source -> Vec<(target, kind, output_index)>
fn build_adjacency(
    connections: &[(String, String, String, usize)],
) -> HashMap<String, Vec<(String, String, usize)>> {
    let mut adj: HashMap<String, Vec<(String, String, usize)>> = HashMap::new();
    for (from, to, kind, output_index) in connections {
        adj.entry(from.clone())
            .or_default()
            .push((to.clone(), kind.clone(), *output_index));
    }
    adj
}

/// Find nodes that have no incoming main connections and participate in at least one connection.
/// Nodes with zero connections are treated as unconnected, not roots.
/// If all connected nodes form cycles (no natural root), pick the first connected node.
fn find_roots(nodes: &[TreeNode], connections: &[(String, String, String, usize)]) -> Vec<String> {
    let has_incoming: HashSet<&str> = connections
        .iter()
        .filter(|(_, _, kind, _)| kind == "main")
        .map(|(_, to, _, _)| to.as_str())
        .collect();
    let participates: HashSet<&str> = connections
        .iter()
        .flat_map(|(from, to, _, _)| [from.as_str(), to.as_str()])
        .collect();
    let roots: Vec<String> = nodes
        .iter()
        .filter(|n| {
            !has_incoming.contains(n.name.as_str()) && participates.contains(n.name.as_str())
        })
        .map(|n| n.name.clone())
        .collect();
    // If no natural roots but there are connections, pick the first connected node to break cycles
    if roots.is_empty()
        && !connections.is_empty()
        && let Some(first) = nodes
            .iter()
            .find(|n| participates.contains(n.name.as_str()))
    {
        return vec![first.name.clone()];
    }
    roots
}

fn format_node(node: &TreeNode) -> String {
    let mut line = format!("{} ({})", node.name, node.type_name());
    for cred in &node.credentials {
        line.push_str(&format!(" [cred: {cred}]"));
    }
    if node.disabled {
        line.push_str(" [disabled]");
    }
    line
}

impl TreeNode {
    fn type_name(&self) -> &str {
        &self.node_type
    }
}

fn derive_label(node_type: &str, output_index: usize, max_outputs: usize) -> Option<String> {
    if max_outputs <= 1 {
        return None;
    }
    if node_type == "n8n-nodes-base.if" {
        return Some(if output_index == 0 {
            "true".to_string()
        } else {
            "false".to_string()
        });
    }
    Some(format!("output {output_index}"))
}

/// Render the execution-flow tree as a string with box-drawing characters.
pub fn render_tree(nodes: &[TreeNode], connections: &[(String, String, String, usize)]) -> String {
    if nodes.is_empty() {
        return "No nodes in workflow".to_string();
    }

    let node_map: HashMap<&str, &TreeNode> = nodes.iter().map(|n| (n.name.as_str(), n)).collect();
    let adj = build_adjacency(connections);
    let roots = find_roots(nodes, connections);

    // Count max outputs per source for label derivation
    let mut max_outputs: HashMap<&str, usize> = HashMap::new();
    for (from, _, _, output_index) in connections {
        let entry = max_outputs.entry(from.as_str()).or_insert(0);
        *entry = (*entry).max(output_index + 1);
    }

    let mut visited = HashSet::new();
    let mut lines = Vec::new();

    for (i, root) in roots.iter().enumerate() {
        if i > 0 {
            lines.push(String::new());
        }
        render_subtree(
            root,
            &node_map,
            &adj,
            &max_outputs,
            &mut visited,
            &mut lines,
            "",
            "",
        );
    }

    // Unconnected nodes
    let connected: HashSet<&str> = visited.iter().map(String::as_str).collect();
    let unconnected: Vec<&TreeNode> = nodes
        .iter()
        .filter(|n| !connected.contains(n.name.as_str()))
        .collect();
    if !unconnected.is_empty() {
        lines.push(String::new());
        lines.push("Unconnected:".to_string());
        for node in &unconnected {
            lines.push(format!("  {}", format_node(node)));
        }
    }

    lines.join("\n")
}

#[allow(clippy::too_many_arguments)]
fn render_subtree(
    name: &str,
    node_map: &HashMap<&str, &TreeNode>,
    adj: &HashMap<String, Vec<(String, String, usize)>>,
    max_outputs: &HashMap<&str, usize>,
    visited: &mut HashSet<String>,
    lines: &mut Vec<String>,
    prefix: &str,
    connector: &str,
) {
    if visited.contains(name) {
        lines.push(format!("{prefix}{connector}{name} (see above)"));
        return;
    }
    visited.insert(name.to_string());

    let node_line = if let Some(node) = node_map.get(name) {
        format_node(node)
    } else {
        name.to_string()
    };
    lines.push(format!("{prefix}{connector}{node_line}"));

    let children: Vec<(String, String, usize)> = adj.get(name).cloned().unwrap_or_default();

    // Separate main and non-main connections
    let main_children: Vec<_> = children.iter().filter(|(_, k, _)| k == "main").collect();
    let other_children: Vec<_> = children.iter().filter(|(_, k, _)| k != "main").collect();

    let child_prefix = if connector.is_empty() {
        String::new()
    } else {
        format!(
            "{}{}",
            prefix,
            if connector.starts_with('\u{251C}') {
                "\u{2502}   "
            } else {
                "    "
            }
        )
    };

    let node_type = node_map
        .get(name)
        .map(|n| n.node_type.as_str())
        .unwrap_or("");
    let max_out = max_outputs.get(name).copied().unwrap_or(1);

    for (idx, (target, _kind, output_index)) in main_children.iter().enumerate() {
        let is_last = idx == main_children.len() - 1 && other_children.is_empty();
        let conn = if is_last {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251C}\u{2500}\u{2500} "
        };
        let label = derive_label(node_type, *output_index, max_out);
        let labeled_conn = if let Some(label) = label {
            format!("{conn}{label} \u{2192} ")
        } else {
            conn.to_string()
        };
        render_subtree(
            target,
            node_map,
            adj,
            max_outputs,
            visited,
            lines,
            &child_prefix,
            &labeled_conn,
        );
    }

    for (idx, (target, kind, _output_index)) in other_children.iter().enumerate() {
        let is_last = idx == other_children.len() - 1;
        let conn = if is_last {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251C}\u{2500}\u{2500} "
        };
        let labeled_conn = format!("{conn}[{kind}] \u{2192} ");
        render_subtree(
            target,
            node_map,
            adj,
            max_outputs,
            visited,
            lines,
            &child_prefix,
            &labeled_conn,
        );
    }
}

/// Build structured tree data for JSON output.
/// Uses DFS from roots to find reachable nodes, consistent with render_tree.
pub fn build_tree_data(
    nodes: &[TreeNode],
    connections: &[(String, String, String, usize)],
) -> TreeData {
    let roots = find_roots(nodes, connections);
    let adj = build_adjacency(connections);
    let edges: Vec<TreeEdge> = connections
        .iter()
        .map(|(from, to, kind, output_index)| {
            let node_type = nodes
                .iter()
                .find(|n| n.name == *from)
                .map(|n| n.node_type.as_str())
                .unwrap_or("");
            let max_out = connections
                .iter()
                .filter(|(f, _, _, _)| f == from)
                .map(|(_, _, _, idx)| idx + 1)
                .max()
                .unwrap_or(1);
            TreeEdge {
                from: from.clone(),
                to: to.clone(),
                kind: kind.clone(),
                label: derive_label(node_type, *output_index, max_out),
            }
        })
        .collect();

    // DFS from roots to find all reachable nodes (same as render_tree)
    let mut reachable = HashSet::new();
    let mut stack: Vec<&str> = roots.iter().map(String::as_str).collect();
    while let Some(name) = stack.pop() {
        if reachable.insert(name.to_string())
            && let Some(children) = adj.get(name)
        {
            for (target, _, _) in children {
                stack.push(target.as_str());
            }
        }
    }

    let unconnected: Vec<String> = nodes
        .iter()
        .filter(|n| !reachable.contains(&n.name))
        .map(|n| n.name.clone())
        .collect();

    TreeData {
        roots,
        edges,
        unconnected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, node_type: &str) -> TreeNode {
        TreeNode {
            name: name.to_string(),
            node_type: node_type.to_string(),
            credentials: vec![],
            disabled: false,
        }
    }

    fn node_with_cred(name: &str, node_type: &str, cred: &str) -> TreeNode {
        TreeNode {
            name: name.to_string(),
            node_type: node_type.to_string(),
            credentials: vec![cred.to_string()],
            disabled: false,
        }
    }

    fn conn(from: &str, to: &str) -> (String, String, String, usize) {
        (from.to_string(), to.to_string(), "main".to_string(), 0)
    }

    fn conn_indexed(from: &str, to: &str, output_index: usize) -> (String, String, String, usize) {
        (
            from.to_string(),
            to.to_string(),
            "main".to_string(),
            output_index,
        )
    }

    #[test]
    fn test_if_branching() {
        let nodes = vec![
            node("Webhook", "n8n-nodes-base.webhook"),
            node("IF", "n8n-nodes-base.if"),
            node("Yes", "n8n-nodes-base.set"),
            node("No", "n8n-nodes-base.noOp"),
        ];
        let conns = vec![
            conn("Webhook", "IF"),
            conn_indexed("IF", "Yes", 0),
            conn_indexed("IF", "No", 1),
        ];
        let result = render_tree(&nodes, &conns);
        assert_eq!(
            result,
            "\
Webhook (n8n-nodes-base.webhook)
└── IF (n8n-nodes-base.if)
    ├── true → Yes (n8n-nodes-base.set)
    └── false → No (n8n-nodes-base.noOp)"
        );
    }

    #[test]
    fn test_cycle_detection() {
        let nodes = vec![
            node("A", "n8n-nodes-base.set"),
            node("B", "n8n-nodes-base.set"),
        ];
        let conns = vec![conn("A", "B"), conn("B", "A")];
        let result = render_tree(&nodes, &conns);
        assert_eq!(
            result,
            "\
A (n8n-nodes-base.set)
└── B (n8n-nodes-base.set)
    └── A (see above)"
        );
    }

    #[test]
    fn test_disconnected_nodes() {
        let nodes = vec![
            node("A", "n8n-nodes-base.set"),
            node("B", "n8n-nodes-base.set"),
            node("Orphan", "n8n-nodes-base.noOp"),
        ];
        let conns = vec![conn("A", "B")];
        let result = render_tree(&nodes, &conns);
        assert_eq!(
            result,
            "\
A (n8n-nodes-base.set)
└── B (n8n-nodes-base.set)

Unconnected:
  Orphan (n8n-nodes-base.noOp)"
        );
    }

    #[test]
    fn test_empty_workflow() {
        let result = render_tree(&[], &[]);
        assert_eq!(result, "No nodes in workflow");
    }

    #[test]
    fn test_credentials_and_disabled() {
        let nodes = vec![
            node("Trigger", "n8n-nodes-base.manualTrigger"),
            node_with_cred("Email", "n8n-nodes-base.emailSend", "Gmail"),
            TreeNode {
                name: "Skip".to_string(),
                node_type: "n8n-nodes-base.noOp".to_string(),
                credentials: vec![],
                disabled: true,
            },
        ];
        let conns = vec![conn("Trigger", "Email"), conn("Trigger", "Skip")];
        let result = render_tree(&nodes, &conns);
        assert!(result.contains("[cred: Gmail]"));
        assert!(result.contains("[disabled]"));
    }

    #[test]
    fn test_multiple_roots() {
        let nodes = vec![
            node("Trigger1", "n8n-nodes-base.manualTrigger"),
            node("Trigger2", "n8n-nodes-base.webhook"),
            node("End", "n8n-nodes-base.noOp"),
        ];
        let conns = vec![conn("Trigger1", "End")];
        let result = render_tree(&nodes, &conns);
        assert!(result.contains("Trigger1"));
        assert!(result.contains("Trigger2"));
    }

    #[test]
    fn test_switch_branching() {
        let nodes = vec![
            node("Trigger", "n8n-nodes-base.manualTrigger"),
            node("Switch", "n8n-nodes-base.switch"),
            node("A", "n8n-nodes-base.set"),
            node("B", "n8n-nodes-base.set"),
        ];
        let conns = vec![
            conn("Trigger", "Switch"),
            conn_indexed("Switch", "A", 0),
            conn_indexed("Switch", "B", 1),
        ];
        let result = render_tree(&nodes, &conns);
        assert!(result.contains("output 0"));
        assert!(result.contains("output 1"));
    }

    #[test]
    fn test_non_main_connections() {
        let nodes = vec![
            node("Agent", "n8n-nodes-base.agent"),
            node("Tool", "n8n-nodes-base.httpRequest"),
        ];
        let conns = vec![(
            "Agent".to_string(),
            "Tool".to_string(),
            "ai_tool".to_string(),
            0,
        )];
        let result = render_tree(&nodes, &conns);
        assert!(result.contains("[ai_tool]"));
    }

    #[test]
    fn test_linear_chain() {
        let nodes = vec![
            node("Trigger", "n8n-nodes-base.manualTrigger"),
            node("Set", "n8n-nodes-base.set"),
            node("End", "n8n-nodes-base.noOp"),
        ];
        let conns = vec![conn("Trigger", "Set"), conn("Set", "End")];
        let result = render_tree(&nodes, &conns);
        assert_eq!(
            result,
            "\
Trigger (n8n-nodes-base.manualTrigger)
\u{2514}\u{2500}\u{2500} Set (n8n-nodes-base.set)
    \u{2514}\u{2500}\u{2500} End (n8n-nodes-base.noOp)"
        );
    }
}
