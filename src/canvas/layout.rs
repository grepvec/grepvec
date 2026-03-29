//! Endpoint-rooted tree layout engine.
//!
//! Each API endpoint is an IO connector on the circuit board. From each
//! endpoint, we trace every downstream dependency as a tree. Nodes are
//! duplicated across trees — the same function appears wherever it's called.
//! This produces clean, non-overlapping diagrams that tell coherent stories.
//!
//! The metaphor: a PCB with discrete signal paths. Each IO connector has
//! its own trace through the board. Shared components (like a voltage
//! regulator) appear on each path that uses them.

use std::collections::{HashMap, HashSet, VecDeque};

// --- Data types ---

#[derive(Debug, Clone)]
pub struct LayoutNode {
    pub id: String,
    pub item_type: String,
    pub name: String,
    pub qualified_name: String,
    pub module_path: String,
    pub repo: String,
    pub file_path: String,
    pub line_start: i32,
    pub visibility: String,
    pub loc: i32,
    pub is_async: bool,
    pub is_boundary: bool,
    pub layer: u32,
    pub block_id: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone)]
pub struct LayoutEdge {
    pub source_id: String,
    pub target_id: String,
    pub edge_type: String,
    pub cross_layer: bool,
    pub length: f32,
}

/// A tree node in an endpoint circuit. May reference the same underlying
/// code item as another tree node (duplication is intentional).
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub tree_id: String,      // unique within this tree: "endpoint::depth::index"
    pub node_id: String,      // references the real LayoutNode id
    pub depth: u32,           // 0 = endpoint (IO pin), 1 = first callees, etc.
    pub children: Vec<usize>, // indices into EndpointTree.nodes
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A complete endpoint circuit: one IO pin and all downstream logic.
#[derive(Debug, Clone)]
pub struct EndpointTree {
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub repo: String,
    pub io_class: crate::canvas::classify::IoClass,
    pub nodes: Vec<TreeNode>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub depth: u32,
}

/// A group of endpoint trees sharing the same IO surface class.
#[derive(Debug, Clone)]
pub struct IoSurfaceGroup {
    pub io_class: crate::canvas::classify::IoClass,
    pub tree_indices: Vec<usize>,  // indices into CircuitLayout.endpoint_trees
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A layer grouping of endpoints for the overview.
#[derive(Debug, Clone)]
pub struct Layer {
    pub index: u32,
    pub name: String,
    pub blocks: Vec<ModuleBlock>,
    pub node_count: usize,
}

/// Retained for compatibility with overview mode.
#[derive(Debug, Clone)]
pub struct ModuleBlock {
    pub id: String,
    pub label: String,
    pub repo: String,
    pub node_ids: Vec<String>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub edge_weight: usize,
}

#[derive(Debug)]
pub struct CircuitLayout {
    pub nodes: HashMap<String, LayoutNode>,
    pub edges: Vec<LayoutEdge>,
    pub layers: Vec<Layer>,
    pub endpoint_trees: Vec<EndpointTree>,
    pub io_groups: Vec<IoSurfaceGroup>,
    pub total_nodes: usize,
    pub total_edges: usize,
}

// --- Constants ---

const NODE_W: f32 = 130.0;
const NODE_H: f32 = 26.0;
const TREE_H_GAP: f32 = 20.0;   // horizontal gap between siblings
const TREE_V_GAP: f32 = 60.0;   // vertical gap between depth levels
const TREE_BOARD_GAP: f32 = 80.0; // gap between endpoint trees on the board

/// Maximum depth to trace from an endpoint (prevents infinite recursion).
const MAX_DEPTH: u32 = 6;

// --- Layout computation ---

pub fn compute_layout(
    mut nodes: HashMap<String, LayoutNode>,
    edges: Vec<LayoutEdge>,
) -> CircuitLayout {
    let total_nodes = nodes.len();
    let total_edges = edges.len();

    // Assign layers
    for (_id, node) in &mut nodes {
        node.layer = classify_layer(node);
    }

    let mut edges: Vec<LayoutEdge> = edges.into_iter().map(|mut e| {
        let sl = nodes.get(&e.source_id).map(|n| n.layer).unwrap_or(3);
        let tl = nodes.get(&e.target_id).map(|n| n.layer).unwrap_or(3);
        e.cross_layer = sl != tl;
        e
    }).collect();

    // Build call graph adjacency: source → [targets]
    let mut call_graph: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &edges {
        if edge.edge_type == "calls" {
            call_graph.entry(edge.source_id.clone())
                .or_default()
                .push(edge.target_id.clone());
        }
    }

    // Classify all nodes into IO surface types
    let io_classes = crate::canvas::classify::classify_all(&nodes, &call_graph);

    // Identify IO endpoints: any node classified as non-Internal with outgoing calls
    let mut endpoints: Vec<String> = nodes.iter()
        .filter(|(id, n)| {
            let class = io_classes.get(*id).copied().unwrap_or(crate::canvas::classify::IoClass::Internal);
            class != crate::canvas::classify::IoClass::Internal
            && n.item_type == "function"
            && call_graph.contains_key(&n.id)
        })
        .map(|(id, _)| id.clone())
        .collect();

    // Sort deterministically by layer (highest first), then by name
    endpoints.sort_by(|a, b| {
        let na = nodes.get(a).unwrap();
        let nb = nodes.get(b).unwrap();
        nb.layer.cmp(&na.layer).then(na.qualified_name.cmp(&nb.qualified_name))
    });

    // Limit to top 40 endpoints for performance
    endpoints.truncate(40);

    // Build endpoint trees
    let mut endpoint_trees: Vec<EndpointTree> = Vec::new();
    let mut board_y: f32 = 0.0;

    for ep_id in &endpoints {
        let ep_node = match nodes.get(ep_id) {
            Some(n) => n,
            None => continue,
        };

        let mut tree_nodes: Vec<TreeNode> = Vec::new();
        let mut visited_in_tree: HashSet<String> = HashSet::new();

        // BFS to build the tree
        let root = TreeNode {
            tree_id: format!("{}::0::0", ep_id),
            node_id: ep_id.clone(),
            depth: 0,
            children: Vec::new(),
            x: 0.0, y: 0.0,
            width: NODE_W, height: NODE_H,
        };
        tree_nodes.push(root);
        visited_in_tree.insert(ep_id.clone());

        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(0); // index of root

        while let Some(parent_idx) = queue.pop_front() {
            let parent_node_id = tree_nodes[parent_idx].node_id.clone();
            let parent_depth = tree_nodes[parent_idx].depth;

            if parent_depth >= MAX_DEPTH { continue; }

            if let Some(callees) = call_graph.get(&parent_node_id) {
                let mut child_indices = Vec::new();
                for callee_id in callees {
                    // Allow duplication across trees but not within a single tree path
                    if visited_in_tree.contains(callee_id) { continue; }
                    if !nodes.contains_key(callee_id) { continue; }

                    let child_idx = tree_nodes.len();
                    let child = TreeNode {
                        tree_id: format!("{}::{}::{}", ep_id, parent_depth + 1, child_idx),
                        node_id: callee_id.clone(),
                        depth: parent_depth + 1,
                        children: Vec::new(),
                        x: 0.0, y: 0.0,
                        width: NODE_W, height: NODE_H,
                    };
                    tree_nodes.push(child);
                    child_indices.push(child_idx);
                    visited_in_tree.insert(callee_id.clone());
                    queue.push_back(child_idx);
                }
                tree_nodes[parent_idx].children = child_indices;
            }
        }

        // Skip trivial trees (endpoint with no callees)
        if tree_nodes.len() < 2 { continue; }

        // Layout the tree: top-down, centered children under parent
        let max_depth = tree_nodes.iter().map(|n| n.depth).max().unwrap_or(0);
        layout_tree(&mut tree_nodes, 0, 0.0, board_y);

        // Compute tree bounding box
        let min_x = tree_nodes.iter().map(|n| n.x).fold(f32::MAX, f32::min);
        let max_x = tree_nodes.iter().map(|n| n.x + n.width).fold(f32::MIN, f32::max);
        let max_y = tree_nodes.iter().map(|n| n.y + n.height).fold(f32::MIN, f32::max);
        let tree_width = max_x - min_x;
        let tree_height = max_y - board_y;

        // Shift tree so min_x = 0
        for tn in &mut tree_nodes {
            tn.x -= min_x;
        }

        let ep_name = ep_node.name.clone();
        let ep_repo = ep_node.repo.clone();
        let ep_io_class = io_classes.get(ep_id).copied()
            .unwrap_or(crate::canvas::classify::IoClass::Internal);

        endpoint_trees.push(EndpointTree {
            endpoint_id: ep_id.clone(),
            endpoint_name: ep_name,
            repo: ep_repo,
            io_class: ep_io_class,
            nodes: tree_nodes,
            x: 0.0,
            y: board_y,
            width: tree_width,
            height: tree_height,
            depth: max_depth,
        });

        board_y += tree_height + TREE_BOARD_GAP;
    }

    // Group trees by IO class and arrange on the board
    use crate::canvas::classify::IoClass;

    let group_order = [
        IoClass::Identity, IoClass::View, IoClass::Action,
        IoClass::Query, IoClass::Ingest, IoClass::Stream,
        IoClass::Operate, IoClass::Schedule,
    ];

    let mut io_groups: Vec<IoSurfaceGroup> = Vec::new();
    let mut by: f32 = 80.0; // leave room for the human node at top
    let group_label_height = 40.0;

    for io_class in &group_order {
        let tree_indices: Vec<usize> = endpoint_trees.iter().enumerate()
            .filter(|(_, t)| t.io_class == *io_class)
            .map(|(i, _)| i)
            .collect();

        if tree_indices.is_empty() { continue; }

        let group_y = by;
        by += group_label_height; // space for group header

        // Arrange this group's trees left-to-right
        let board_max_width = 3000.0;
        let mut bx: f32 = 20.0;
        let mut row_height: f32 = 0.0;
        let mut group_max_x: f32 = 0.0;

        for &ti in &tree_indices {
            let tree = &mut endpoint_trees[ti];
            if bx + tree.width > board_max_width && bx > 20.0 {
                bx = 20.0;
                by += row_height + TREE_BOARD_GAP;
                row_height = 0.0;
            }

            let dx = bx - tree.x;
            let dy = by - tree.y;
            for tn in &mut tree.nodes {
                tn.x += dx;
                tn.y += dy;
            }
            tree.x = bx;
            tree.y = by;

            bx += tree.width + TREE_BOARD_GAP;
            group_max_x = group_max_x.max(bx);
            row_height = row_height.max(tree.height);
        }

        by += row_height;
        let group_height = by - group_y;

        io_groups.push(IoSurfaceGroup {
            io_class: *io_class,
            tree_indices,
            x: 0.0,
            y: group_y,
            width: group_max_x,
            height: group_height,
        });

        by += TREE_BOARD_GAP * 1.5; // extra gap between IO groups
    }

    // Build layers for overview (retain compatibility)
    let max_layer = nodes.values().map(|n| n.layer).max().unwrap_or(5);
    let mut layers = Vec::new();
    for idx in 0..=max_layer {
        let count = nodes.values().filter(|n| n.layer == idx).count();
        layers.push(Layer {
            index: idx,
            name: layer_name(idx).to_string(),
            blocks: Vec::new(),
            node_count: count,
        });
    }

    // Compute edge lengths
    for edge in &mut edges {
        let (sx, sy) = nodes.get(&edge.source_id).map(|n| (n.x + n.width/2.0, n.y + n.height/2.0)).unwrap_or((0.0, 0.0));
        let (tx, ty) = nodes.get(&edge.target_id).map(|n| (n.x + n.width/2.0, n.y + n.height/2.0)).unwrap_or((0.0, 0.0));
        edge.length = ((tx-sx)*(tx-sx) + (ty-sy)*(ty-sy)).sqrt();
    }

    CircuitLayout {
        nodes,
        edges,
        layers,
        endpoint_trees,
        io_groups,
        total_nodes,
        total_edges,
    }
}

/// Recursive tree layout: place node, then center children below it. (public for drill-down)
pub fn layout_tree_pub(nodes: &mut Vec<TreeNode>, idx: usize, x: f32, y: f32) -> f32 {
    layout_tree(nodes, idx, x, y)
}

/// Build endpoint trees from an arbitrary selection of node IDs.
/// Uses the same tree-building logic as compute_layout but filtered to the selection.
pub fn build_selection_trees(
    selected_ids: &[String],
    full_layout: &CircuitLayout,
) -> Vec<EndpointTree> {
    let sel_set: HashSet<&str> = selected_ids.iter().map(|s| s.as_str()).collect();

    // Build call graph within the selection
    let mut call_graph: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut has_incoming: HashSet<&str> = HashSet::new();
    for edge in &full_layout.edges {
        if edge.edge_type == "calls"
            && sel_set.contains(edge.source_id.as_str())
            && sel_set.contains(edge.target_id.as_str())
        {
            call_graph.entry(edge.source_id.as_str())
                .or_default()
                .push(edge.target_id.as_str());
            has_incoming.insert(edge.target_id.as_str());
        }
    }

    // Classify IO types for selected nodes
    let owned_call_graph: HashMap<String, Vec<String>> = call_graph.iter()
        .map(|(&k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
        .collect();
    let io_classes = crate::canvas::classify::classify_all(&full_layout.nodes, &owned_call_graph);

    // Roots: nodes with outgoing calls but no incoming calls within the selection,
    // or all selected nodes if none have calls (each becomes a single-node tree)
    let mut roots: Vec<&str> = selected_ids.iter()
        .map(|s| s.as_str())
        .filter(|id| call_graph.contains_key(id) && !has_incoming.contains(id))
        .collect();

    // Sort by layer (highest first) then name for determinism
    roots.sort_by(|a, b| {
        let la = full_layout.nodes.get(*a).map(|n| n.layer).unwrap_or(0);
        let lb = full_layout.nodes.get(*b).map(|n| n.layer).unwrap_or(0);
        lb.cmp(&la).then(a.cmp(b))
    });

    // Also include orphan nodes (no calls edges at all within selection) as single-node trees
    let in_any_tree: HashSet<&str> = {
        let mut s = HashSet::new();
        for &root in &roots {
            s.insert(root);
            if let Some(callees) = call_graph.get(root) {
                // BFS to collect all reachable
                let mut queue: VecDeque<&str> = callees.iter().copied().collect();
                while let Some(n) = queue.pop_front() {
                    if s.insert(n) {
                        if let Some(next) = call_graph.get(n) {
                            queue.extend(next.iter().copied());
                        }
                    }
                }
            }
        }
        s
    };
    let orphans: Vec<&str> = selected_ids.iter()
        .map(|s| s.as_str())
        .filter(|id| !in_any_tree.contains(id))
        .collect();

    let mut endpoint_trees: Vec<EndpointTree> = Vec::new();
    let mut board_y: f32 = 0.0;

    // Build proper trees from roots
    for &root_id in &roots {
        let ep_node = match full_layout.nodes.get(root_id) {
            Some(n) => n,
            None => continue,
        };

        let mut tree_nodes: Vec<TreeNode> = Vec::new();
        let mut visited: HashSet<&str> = HashSet::new();

        tree_nodes.push(TreeNode {
            tree_id: format!("sel::{}::0::0", root_id),
            node_id: root_id.to_string(),
            depth: 0,
            children: Vec::new(),
            x: 0.0, y: 0.0,
            width: NODE_W, height: NODE_H,
        });
        visited.insert(root_id);

        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(0);

        while let Some(parent_idx) = queue.pop_front() {
            let pid = tree_nodes[parent_idx].node_id.clone();
            let pdepth = tree_nodes[parent_idx].depth;
            if pdepth >= 6 { continue; }

            if let Some(callees) = call_graph.get(pid.as_str()) {
                let mut child_indices = Vec::new();
                for &callee in callees {
                    if !sel_set.contains(callee) { continue; }
                    if visited.contains(callee) { continue; }
                    let ci = tree_nodes.len();
                    tree_nodes.push(TreeNode {
                        tree_id: format!("sel::{}::{}::{}", root_id, pdepth + 1, ci),
                        node_id: callee.to_string(),
                        depth: pdepth + 1,
                        children: Vec::new(),
                        x: 0.0, y: 0.0,
                        width: NODE_W, height: NODE_H,
                    });
                    child_indices.push(ci);
                    visited.insert(callee);
                    queue.push_back(ci);
                }
                tree_nodes[parent_idx].children = child_indices;
            }
        }

        if tree_nodes.len() < 2 { continue; } // skip single-node trees, handle as orphans

        let max_depth = tree_nodes.iter().map(|n| n.depth).max().unwrap_or(0);
        layout_tree(&mut tree_nodes, 0, 0.0, board_y);

        let min_x = tree_nodes.iter().map(|n| n.x).fold(f32::MAX, f32::min);
        let max_x = tree_nodes.iter().map(|n| n.x + n.width).fold(f32::MIN, f32::max);
        let max_y = tree_nodes.iter().map(|n| n.y + n.height).fold(f32::MIN, f32::max);
        for tn in &mut tree_nodes { tn.x -= min_x; }

        let io_class = io_classes.get(root_id).copied()
            .unwrap_or(crate::canvas::classify::IoClass::Internal);

        endpoint_trees.push(EndpointTree {
            endpoint_id: root_id.to_string(),
            endpoint_name: ep_node.name.clone(),
            repo: ep_node.repo.clone(),
            io_class,
            nodes: tree_nodes,
            x: 0.0, y: board_y,
            width: max_x - min_x,
            height: max_y - board_y,
            depth: max_depth,
        });
        board_y += (max_y - board_y) + TREE_BOARD_GAP;
    }

    // Add orphans as single-node "trees" in a grid
    if !orphans.is_empty() {
        let cols = ((orphans.len() as f32).sqrt().ceil() as usize).max(1);
        for (i, &oid) in orphans.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let ox = col as f32 * (NODE_W + TREE_H_GAP);
            let oy = board_y + row as f32 * (NODE_H + TREE_V_GAP);

            let ep_node = match full_layout.nodes.get(oid) {
                Some(n) => n,
                None => continue,
            };
            let io_class = io_classes.get(oid).copied()
                .unwrap_or(crate::canvas::classify::IoClass::Internal);

            endpoint_trees.push(EndpointTree {
                endpoint_id: oid.to_string(),
                endpoint_name: ep_node.name.clone(),
                repo: ep_node.repo.clone(),
                io_class,
                nodes: vec![TreeNode {
                    tree_id: format!("sel::{}::0::0", oid),
                    node_id: oid.to_string(),
                    depth: 0,
                    children: Vec::new(),
                    x: ox, y: oy,
                    width: NODE_W, height: NODE_H,
                }],
                x: ox, y: oy,
                width: NODE_W, height: NODE_H,
                depth: 0,
            });
        }
    }

    endpoint_trees
}

fn layout_tree(nodes: &mut Vec<TreeNode>, idx: usize, x: f32, y: f32) -> f32 {
    nodes[idx].y = y;

    let children = nodes[idx].children.clone();

    if children.is_empty() {
        // Leaf node
        nodes[idx].x = x;
        return NODE_W; // return width consumed
    }

    // Layout children left-to-right
    let child_y = y + NODE_H + TREE_V_GAP;
    let mut child_x = x;
    let mut total_children_width: f32 = 0.0;

    for (i, &child_idx) in children.iter().enumerate() {
        let child_width = layout_tree(nodes, child_idx, child_x, child_y);
        child_x += child_width + TREE_H_GAP;
        total_children_width += child_width;
        if i > 0 { total_children_width += TREE_H_GAP; }
    }

    // Center parent over children
    let first_child_center = nodes[children[0]].x + NODE_W / 2.0;
    let last_child_center = nodes[*children.last().unwrap()].x + NODE_W / 2.0;
    let center = (first_child_center + last_child_center) / 2.0;
    nodes[idx].x = center - NODE_W / 2.0;

    total_children_width.max(NODE_W)
}

fn classify_layer(node: &LayoutNode) -> u32 {
    let path = node.module_path.to_lowercase();
    let file = node.file_path.to_lowercase();
    if node.is_boundary { return 0; }
    if path.contains("pages") || path.contains("components") || path.contains("frontend") || file.contains("frontend") { return 5; }
    if path.contains("routes") || path.contains("server_fns") || path.contains("api::v1") || file.contains("server_fns") { return 4; }
    if path.contains("grpc") || path.contains("service") || path.contains("grpc_client") || file.contains("grpc") { return 3; }
    if path.contains("storage") || path.contains("qdrant") || path.contains("backup") || path.contains("cache") || file.contains("storage") || file.contains("backup") { return 2; }
    if path.contains("config") || path.contains("auth") || path.contains("hmac") || path.contains("middleware") || path.contains("metrics") || path.contains("audit") || path.contains("error") || path.contains("state") || path.contains("rate") || file.contains("config") || file.contains("auth") { return 1; }
    3
}

fn layer_name(index: u32) -> &'static str {
    match index { 0 => "Substrate", 1 => "Infrastructure", 2 => "Data Access", 3 => "Service Logic", 4 => "API Surface", 5 => "Frontend", _ => "Unknown" }
}
