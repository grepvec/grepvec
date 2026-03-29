//! 3D Force-Directed Sphere Graph.
//!
//! Every node rendered as a sphere sized by connection count.
//! Layout uses ForceAtlas2: degree-weighted repulsion, edge attraction,
//! gravity, adaptive per-node speed, and module-based clustering.

use crate::canvas::layout::CircuitLayout;
use std::collections::HashMap;

// ── Vec3 ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0, z: 0.0 };

    #[inline]
    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    #[inline]
    pub fn length_sq(self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    #[inline]
    pub fn normalize(self) -> Self {
        let len = self.length();
        if len < 1e-10 {
            return Self::ZERO;
        }
        Self {
            x: self.x / len,
            y: self.y / len,
            z: self.z / len,
        }
    }
}

impl std::ops::Add for Vec3 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self { x: self.x + rhs.x, y: self.y + rhs.y, z: self.z + rhs.z }
    }
}
impl std::ops::Sub for Vec3 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self { x: self.x - rhs.x, y: self.y - rhs.y, z: self.z - rhs.z }
    }
}
impl std::ops::Mul<f32> for Vec3 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self { x: self.x * s, y: self.y * s, z: self.z * s }
    }
}
impl std::ops::AddAssign for Vec3 {
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
        self.z += rhs.z;
    }
}

// ── Data types ──────────────────────────────────────────────────────────

pub struct SphereNode {
    pub id: String,
    pub pos: Vec3,
    pub radius: f32,     // world-space radius (3–15 units)
    pub degree: u32,
    pub group: String,   // module_path — drives clustering
    pub item_type: String,
    pub layer: u32,
    pub color: [u8; 3],
}

pub struct SphereEdge {
    pub source_idx: usize,
    pub target_idx: usize,
    pub edge_type: String,
}

pub struct SphereLayout {
    pub nodes: Vec<SphereNode>,
    pub edges: Vec<SphereEdge>,
    pub bounding_radius: f32,
}

// ── Camera ─────────────────────────────────────────────────────────────

/// 3D camera for orbiting a scene.
pub struct Camera3D {
    pub yaw: f32,        // horizontal rotation (radians)
    pub pitch: f32,      // vertical rotation (radians)
    pub distance: f32,   // distance from center
    pub center: Vec3,    // look-at point
    pub fov: f32,        // field of view (radians)
}

impl Camera3D {
    pub fn new() -> Self {
        Self {
            yaw: 0.4,
            pitch: 0.3,
            distance: 800.0,
            center: Vec3 { x: 0.0, y: 0.0, z: 0.0 },
            fov: std::f32::consts::FRAC_PI_4,
        }
    }

    /// Project a 3D point to 2D screen coordinates.
    pub fn project(&self, p: Vec3, screen_w: f32, screen_h: f32) -> Option<(f32, f32, f32)> {
        let cam_x = self.center.x + self.distance * self.yaw.cos() * self.pitch.cos();
        let cam_y = self.center.y + self.distance * self.pitch.sin();
        let cam_z = self.center.z + self.distance * self.yaw.sin() * self.pitch.cos();

        let dx = self.center.x - cam_x;
        let dy = self.center.y - cam_y;
        let dz = self.center.z - cam_z;
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        let fwd = Vec3 { x: dx / len, y: dy / len, z: dz / len };

        let world_up = Vec3 { x: 0.0, y: 1.0, z: 0.0 };
        let right = cross(fwd, world_up);
        let right_len = (right.x * right.x + right.y * right.y + right.z * right.z).sqrt();
        let right = Vec3 { x: right.x / right_len, y: right.y / right_len, z: right.z / right_len };
        let up = cross(right, fwd);

        let rx = p.x - cam_x;
        let ry = p.y - cam_y;
        let rz = p.z - cam_z;

        let view_z = rx * fwd.x + ry * fwd.y + rz * fwd.z;
        if view_z < 0.1 { return None; }

        let view_x = rx * right.x + ry * right.y + rz * right.z;
        let view_y = rx * up.x + ry * up.y + rz * up.z;

        let fov_scale = 1.0 / (self.fov / 2.0).tan();
        let sx = screen_w / 2.0 + (view_x / view_z) * fov_scale * screen_h / 2.0;
        let sy = screen_h / 2.0 - (view_y / view_z) * fov_scale * screen_h / 2.0;

        Some((sx, sy, view_z))
    }
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    Vec3 {
        x: a.y * b.z - a.z * b.y,
        y: a.z * b.x - a.x * b.z,
        z: a.x * b.y - a.y * b.x,
    }
}

// ── Pinned examination board ────────────────────────────────────────────

/// A node inside a pinned examination board.
pub struct PinNode {
    pub id: String,
    pub name: String,
    pub item_type: String,
    pub layer: u32,
    pub color: [u8; 3],
    pub degree: u32,
    /// Position within the board content area.
    pub bx: f32,
    pub by: f32,
    pub bw: f32,
    pub bh: f32,
}

/// A floating 2D sub-graph pinned to a 3D region of the sphere view.
pub struct PinnedBoard {
    pub anchor: Vec3,
    pub nodes: Vec<PinNode>,
    pub edges: Vec<(usize, usize)>,
    pub board_w: f32,
    pub board_h: f32,
    /// (layer, label_name, y_offset within content area)
    pub layer_labels: Vec<(u32, String, f32)>,
    /// Screen-space offset from projected anchor to board top-left.
    pub offset: (f32, f32),
}

const LAYER_NAMES: [&str; 6] = [
    "Substrate", "Infrastructure", "Data Access",
    "Service Logic", "API Surface", "Frontend",
];

/// Build a pinned examination board from selected node indices.
pub fn build_pinned_board(
    selected_indices: &[usize],
    sphere: &SphereLayout,
    circuit: &CircuitLayout,
) -> PinnedBoard {
    use std::collections::HashSet;

    let sel_set: HashSet<usize> = selected_indices.iter().copied().collect();

    // 3D centroid
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    let mut cz = 0.0_f32;
    for &i in selected_indices {
        cx += sphere.nodes[i].pos.x;
        cy += sphere.nodes[i].pos.y;
        cz += sphere.nodes[i].pos.z;
    }
    let n = selected_indices.len().max(1) as f32;
    let anchor = Vec3 { x: cx / n, y: cy / n, z: cz / n };

    // Group by layer, sort within layer by module then name
    let mut by_layer: HashMap<u32, Vec<usize>> = HashMap::new();
    for &i in selected_indices {
        by_layer.entry(sphere.nodes[i].layer).or_default().push(i);
    }
    for nodes in by_layer.values_mut() {
        nodes.sort_by(|&a, &b| {
            sphere.nodes[a].group.cmp(&sphere.nodes[b].group)
                .then(sphere.nodes[a].id.cmp(&sphere.nodes[b].id))
        });
    }
    let mut layers: Vec<u32> = by_layer.keys().copied().collect();
    layers.sort_unstable();
    layers.reverse(); // top layer (Frontend) first, Substrate last

    // Layout constants — match Board view sizing
    let label_w = 110.0_f32;
    let node_w = 130.0_f32;
    let node_h = 26.0_f32;
    let h_gap = 20.0_f32;
    let v_gap = 40.0_f32;

    let mut pin_nodes = Vec::new();
    let mut sphere_idx_to_pin: HashMap<usize, usize> = HashMap::new();
    let mut layer_labels = Vec::new();
    let mut row_y = 0.0_f32;
    let mut max_width = 0.0_f32;

    for &layer in &layers {
        let row = &by_layer[&layer];
        let label = LAYER_NAMES.get(layer as usize).unwrap_or(&"?");
        layer_labels.push((layer, label.to_string(), row_y + 2.0));

        let row_width = label_w + row.len() as f32 * (node_w + h_gap) - h_gap;
        max_width = max_width.max(row_width);

        for (col, &si) in row.iter().enumerate() {
            let sn = &sphere.nodes[si];
            let ln = circuit.nodes.get(&sn.id);
            let name = ln
                .map(|l| l.name.clone())
                .unwrap_or_else(|| sn.id.rsplit("::").next().unwrap_or("?").to_string());

            sphere_idx_to_pin.insert(si, pin_nodes.len());
            pin_nodes.push(PinNode {
                id: sn.id.clone(),
                name,
                item_type: sn.item_type.clone(),
                layer: sn.layer,
                color: sn.color,
                degree: sn.degree,
                bx: label_w + col as f32 * (node_w + h_gap),
                by: row_y,
                bw: node_w,
                bh: node_h,
            });
        }
        row_y += node_h + v_gap;
    }

    let board_w = max_width.max(120.0);
    let board_h = (row_y - v_gap).max(node_h);

    // Edges between selected nodes
    let mut edges = Vec::new();
    for se in &sphere.edges {
        if sel_set.contains(&se.source_idx) && sel_set.contains(&se.target_idx) {
            if let (Some(&pi), Some(&qi)) =
                (sphere_idx_to_pin.get(&se.source_idx), sphere_idx_to_pin.get(&se.target_idx))
            {
                edges.push((pi, qi));
            }
        }
    }

    PinnedBoard {
        anchor,
        nodes: pin_nodes,
        edges,
        board_w,
        board_h,
        layer_labels,
        offset: (130.0, -40.0),
    }
}

/// Filter a selection down to its largest connected component.
/// Discards outlier nodes that aren't structurally connected to the bulk.
pub fn filter_to_core(selected: &[usize], sphere: &SphereLayout) -> Vec<usize> {
    if selected.len() <= 2 { return selected.to_vec(); }

    let sel_set: std::collections::HashSet<usize> = selected.iter().copied().collect();

    // Build adjacency within the selection
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for se in &sphere.edges {
        if sel_set.contains(&se.source_idx) && sel_set.contains(&se.target_idx) {
            adj.entry(se.source_idx).or_default().push(se.target_idx);
            adj.entry(se.target_idx).or_default().push(se.source_idx);
        }
    }

    // Find connected components via BFS
    let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut largest: Vec<usize> = Vec::new();

    for &node in selected {
        if visited.contains(&node) { continue; }
        let mut component = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(node);
        visited.insert(node);
        while let Some(n) = queue.pop_front() {
            component.push(n);
            if let Some(neighbors) = adj.get(&n) {
                for &nb in neighbors {
                    if visited.insert(nb) {
                        queue.push_back(nb);
                    }
                }
            }
        }
        if component.len() > largest.len() {
            largest = component;
        }
    }

    largest
}

/// Build a focused sphere graph from selected indices of a parent sphere.
/// Extracts the subgraph and runs ForceAtlas2 on just those nodes.
pub fn build_focus_sphere(selected: &[usize], parent: &SphereLayout) -> SphereLayout {
    let _sel_set: std::collections::HashSet<usize> = selected.iter().copied().collect();

    // Map parent indices → focus indices
    let mut parent_to_focus: HashMap<usize, usize> = HashMap::new();
    let mut nodes: Vec<SphereNode> = Vec::with_capacity(selected.len());

    let n = selected.len();
    let golden_angle = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    let initial_radius = (n as f32).cbrt() * 30.0;

    // Compute degree within the selection
    let mut degrees = vec![0u32; n];
    let mut edges = Vec::new();

    for (fi, &pi) in selected.iter().enumerate() {
        parent_to_focus.insert(pi, fi);
    }
    for se in &parent.edges {
        if let (Some(&fi), Some(&ti)) = (
            parent_to_focus.get(&se.source_idx),
            parent_to_focus.get(&se.target_idx),
        ) {
            degrees[fi] += 1;
            degrees[ti] += 1;
            edges.push(SphereEdge {
                source_idx: fi,
                target_idx: ti,
                edge_type: se.edge_type.clone(),
            });
        }
    }

    let max_degree = degrees.iter().copied().max().unwrap_or(1).max(1) as f32;
    let r_min = 4.0_f32;
    let r_max = 20.0_f32;

    for (fi, &pi) in selected.iter().enumerate() {
        let pn = &parent.nodes[pi];
        let deg = degrees[fi];

        // Fibonacci sphere placement
        let y_norm = 1.0 - (2.0 * fi as f32 + 1.0) / n.max(1) as f32;
        let r_ring = (1.0 - y_norm * y_norm).sqrt();
        let theta = golden_angle * fi as f32;

        nodes.push(SphereNode {
            id: pn.id.clone(),
            pos: Vec3 {
                x: r_ring * theta.cos() * initial_radius,
                y: y_norm * initial_radius,
                z: r_ring * theta.sin() * initial_radius,
            },
            radius: r_min + (r_max - r_min) * (deg as f32 / max_degree).sqrt(),
            degree: deg,
            group: pn.group.clone(),
            item_type: pn.item_type.clone(),
            layer: pn.layer,
            color: pn.color,
        });
    }

    // Run force-directed layout
    run_forceatlas2(&mut nodes, &edges);

    let bounding_radius = nodes.iter()
        .map(|n| n.pos.length() + n.radius)
        .fold(0.0_f32, f32::max);

    SphereLayout { nodes, edges, bounding_radius }
}

/// Layer color palette — matches grepvec's 6-layer architecture.
pub const LAYER_COLORS: [[u8; 3]; 6] = [
    [100, 150, 200], // 0 Substrate     — steel blue
    [80, 190, 140],  // 1 Infrastructure — teal
    [200, 180, 80],  // 2 Data Access    — gold
    [220, 120, 70],  // 3 Service Logic  — coral
    [130, 100, 220], // 4 API Surface    — indigo
    [200, 80, 170],  // 5 Frontend       — magenta
];

// ── Layout construction ─────────────────────────────────────────────────

/// Build a force-directed sphere layout from the circuit graph.
pub fn build_sphere_layout(layout: &CircuitLayout) -> SphereLayout {
    let node_ids: Vec<&String> = layout.nodes.keys().collect();
    let n = node_ids.len();
    let id_to_idx: HashMap<&str, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();

    // Degree per node
    let mut degrees = vec![0u32; n];
    let mut edges = Vec::new();

    for edge in &layout.edges {
        if let (Some(&si), Some(&ti)) = (
            id_to_idx.get(edge.source_id.as_str()),
            id_to_idx.get(edge.target_id.as_str()),
        ) {
            degrees[si] += 1;
            degrees[ti] += 1;
            edges.push(SphereEdge {
                source_idx: si,
                target_idx: ti,
                edge_type: edge.edge_type.clone(),
            });
        }
    }

    let max_degree = degrees.iter().copied().max().unwrap_or(1).max(1) as f32;

    // Radius: sqrt scaling, 3–15 world units
    let r_min = 3.0_f32;
    let r_max = 15.0_f32;

    // Fibonacci sphere initial placement — even distribution in 3D
    let golden_angle = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    let initial_radius = (n as f32).cbrt() * 40.0;

    let mut nodes: Vec<SphereNode> = Vec::with_capacity(n);

    for (i, id) in node_ids.iter().enumerate() {
        let ln = &layout.nodes[*id];
        let deg = degrees[i];

        let y_norm = 1.0 - (2.0 * i as f32 + 1.0) / n as f32;
        let r_ring = (1.0 - y_norm * y_norm).sqrt();
        let theta = golden_angle * i as f32;

        let pos = Vec3 {
            x: r_ring * theta.cos() * initial_radius,
            y: y_norm * initial_radius,
            z: r_ring * theta.sin() * initial_radius,
        };

        let radius = r_min + (r_max - r_min) * (deg as f32 / max_degree).sqrt();
        let color = LAYER_COLORS
            .get(ln.layer as usize)
            .copied()
            .unwrap_or([120, 120, 120]);

        nodes.push(SphereNode {
            id: (*id).clone(),
            pos,
            radius,
            degree: deg,
            group: ln.module_path.clone(),
            item_type: ln.item_type.clone(),
            layer: ln.layer,
            color,
        });
    }

    // Force-directed layout
    run_forceatlas2(&mut nodes, &edges);

    // Bounding radius for camera framing
    let bounding_radius = nodes
        .iter()
        .map(|n| n.pos.length() + n.radius)
        .fold(0.0_f32, f32::max);

    SphereLayout {
        nodes,
        edges,
        bounding_radius,
    }
}

// ── ForceAtlas2 ─────────────────────────────────────────────────────────

fn run_forceatlas2(nodes: &mut [SphereNode], edges: &[SphereEdge]) {
    let n = nodes.len();
    if n == 0 {
        return;
    }

    // FA2 parameters
    let k_r: f32 = 1.0;   // repulsion scale
    let k_g: f32 = 1.0;   // gravity
    let k_s: f32 = 0.1;   // speed constant
    let tolerance: f32 = 1.0;
    let intra_group_boost: f32 = 3.0; // stronger attraction within same module
    let iterations = 80;

    let masses: Vec<f32> = nodes.iter().map(|n| n.degree as f32 + 1.0).collect();
    let mut forces = vec![Vec3::ZERO; n];
    let mut prev_forces = vec![Vec3::ZERO; n];

    for iter in 0..iterations {
        // Reset
        for f in forces.iter_mut() {
            *f = Vec3::ZERO;
        }

        // Adaptive repulsion cutoff — skip distant pairs after initial spread
        let cutoff_sq = if iter < 20 {
            f32::MAX
        } else {
            let sample_count = edges.len().min(500).max(1);
            let avg_edge_dist: f32 = edges
                .iter()
                .take(sample_count)
                .map(|e| (nodes[e.source_idx].pos - nodes[e.target_idx].pos).length())
                .sum::<f32>()
                / sample_count as f32;
            (avg_edge_dist * 4.0).powi(2).max(10000.0)
        };

        // 1. Repulsion (degree-weighted, all pairs)
        for i in 0..n {
            let pi = nodes[i].pos;
            let mi = masses[i];
            for j in (i + 1)..n {
                let diff = pi - nodes[j].pos;
                let dist_sq = diff.length_sq();
                if dist_sq > cutoff_sq {
                    continue;
                }
                let dist = dist_sq.sqrt().max(0.1);
                let force_mag = k_r * mi * masses[j] / dist;
                let fv = diff.normalize() * force_mag;
                forces[i] += fv;
                forces[j].x -= fv.x;
                forces[j].y -= fv.y;
                forces[j].z -= fv.z;
            }
        }

        // 2. Attraction (edges, degree-weighted denominator)
        for edge in edges {
            let s = edge.source_idx;
            let t = edge.target_idx;
            let diff = nodes[t].pos - nodes[s].pos;
            let dist = diff.length().max(0.1);
            let dir = diff.normalize();

            let w = if nodes[s].group == nodes[t].group {
                intra_group_boost
            } else {
                1.0
            };

            let fs = dir * (w * dist / masses[s]);
            let ft = dir * (w * dist / masses[t]);
            forces[s] += fs;
            forces[t].x -= ft.x;
            forces[t].y -= ft.y;
            forces[t].z -= ft.z;
        }

        // 3. Gravity (strong: constant force toward origin)
        for i in 0..n {
            let dir = nodes[i].pos.normalize();
            forces[i].x -= dir.x * k_g * masses[i];
            forces[i].y -= dir.y * k_g * masses[i];
            forces[i].z -= dir.z * k_g * masses[i];
        }

        // 4. Adaptive speed (FA2 swing/traction)
        let mut global_swing: f32 = 0.0;
        let mut global_traction: f32 = 0.0;
        for i in 0..n {
            let swing_vec = Vec3 {
                x: forces[i].x - prev_forces[i].x,
                y: forces[i].y - prev_forces[i].y,
                z: forces[i].z - prev_forces[i].z,
            };
            let tract_vec = Vec3 {
                x: (forces[i].x + prev_forces[i].x) * 0.5,
                y: (forces[i].y + prev_forces[i].y) * 0.5,
                z: (forces[i].z + prev_forces[i].z) * 0.5,
            };
            global_swing += masses[i] * swing_vec.length();
            global_traction += masses[i] * tract_vec.length();
        }
        let global_speed = tolerance * global_traction / global_swing.max(0.001);

        // 5. Apply displacement
        for i in 0..n {
            let swing = Vec3 {
                x: forces[i].x - prev_forces[i].x,
                y: forces[i].y - prev_forces[i].y,
                z: forces[i].z - prev_forces[i].z,
            }
            .length();
            let traction = Vec3 {
                x: (forces[i].x + prev_forces[i].x) * 0.5,
                y: (forces[i].y + prev_forces[i].y) * 0.5,
                z: (forces[i].z + prev_forces[i].z) * 0.5,
            }
            .length();

            let node_speed = k_s * traction / (traction + swing).max(0.001);
            let speed = node_speed.min(global_speed / masses[i]);
            let flen = forces[i].length();

            if flen > 0.001 {
                let disp = forces[i].normalize() * (speed * flen);
                nodes[i].pos.x += disp.x;
                nodes[i].pos.y += disp.y;
                nodes[i].pos.z += disp.z;
            }
            prev_forces[i] = forces[i];
        }
    }
}
