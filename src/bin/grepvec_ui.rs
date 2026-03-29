//! grepvec Canvas — 3D Force-Directed Code Graph.

use eframe::egui;
use grepvec::canvas::layout::CircuitLayout;
use grepvec::canvas::sphere_view::{Camera3D, SphereLayout, Vec3};

fn main() -> eframe::Result<()> {
    let db_url = std::env::var("TOWER_DB_URL").unwrap_or_default();
    if db_url.is_empty() {
        eprintln!("Error: TOWER_DB_URL not set");
        std::process::exit(1);
    }

    println!("Loading code graph...");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let layout = rt.block_on(async {
        let pool = sqlx::PgPool::connect(&db_url).await.expect("DB connection failed");
        grepvec::canvas::load_layout(&pool).await.expect("Layout failed")
    });

    println!(
        "Loaded {} nodes, {} edges, {} endpoint trees. Launching...",
        layout.total_nodes, layout.total_edges, layout.endpoint_trees.len()
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("grepvec")
            .with_app_id("grepvec".to_string())
            .with_decorations(false)
            .with_maximized(true),
        ..Default::default()
    };

    eframe::run_native(
        "grepvec",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(ShiftApp::new(layout)))
        }),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum ColorMode { Layer, IoSurface }

struct ShiftApp {
    layout: CircuitLayout,
    // Sphere
    sphere_layout: SphereLayout,
    sphere_camera: Camera3D,
    // Focus sphere (drill-in from area selection)
    focus_sphere: Option<SphereLayout>,
    focus_camera: Camera3D,
    // Selection
    selected_node: Option<String>,
    hovered_node_id: Option<String>,
    // Area selection tool
    area_select_active: bool,
    area_select_origin: Option<egui::Pos2>,
    area_select_radius: f32,
    area_highlighted: Vec<usize>,
    // Search
    search_query: String,
    search_results: Vec<String>,
    // Animation
    anim_time: f32,
    // Toolbar visibility
    toolbar_visible: bool,
    toolbar_timer: f32,
    // Color mode
    color_mode: ColorMode,
    // Quit: consecutive Ctrl+C
    quit_pending: bool,
    quit_timer: f32,
}

impl ShiftApp {
    fn new(layout: CircuitLayout) -> Self {
        let sphere = grepvec::canvas::sphere_view::build_sphere_layout(&layout);
        let mut camera = Camera3D::new();
        camera.distance = sphere.bounding_radius * 2.5;
        Self {
            layout,
            sphere_layout: sphere,
            sphere_camera: camera,
            focus_sphere: None,
            focus_camera: Camera3D::new(),
            selected_node: None,
            hovered_node_id: None,
            area_select_active: false,
            area_select_origin: None,
            area_select_radius: 0.0,
            area_highlighted: Vec::new(),
            search_query: String::new(),
            search_results: Vec::new(),
            anim_time: 0.0,
            toolbar_visible: false,
            toolbar_timer: 0.0,
            color_mode: ColorMode::Layer,
            quit_pending: false,
            quit_timer: 0.0,
        }
    }
}

impl eframe::App for ShiftApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Continuous repaint for animations and timers
        ctx.request_repaint();

        // Consecutive Escape to quit
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if esc {
            if self.quit_pending {
                std::process::exit(0);
            } else {
                self.quit_pending = true;
                self.quit_timer = 2.0;
            }
        }
        if self.quit_pending {
            self.quit_timer -= ctx.input(|i| i.unstable_dt);
            if self.quit_timer <= 0.0 {
                self.quit_pending = false;
            }
        }

        // Advance animation always (for energy flow + idle rotation)
        self.anim_time += ctx.input(|i| i.unstable_dt);

        // --- Floating toolbar (appears when mouse near top) ---
        let mouse_near_top = ctx.input(|i| {
            i.pointer.hover_pos().map(|p| p.y < 60.0).unwrap_or(false)
        });

        if mouse_near_top || self.toolbar_visible {
            self.toolbar_visible = true;
            self.toolbar_timer = 3.0;

            egui::Window::new("toolbar")
                .title_bar(false)
                .fixed_pos(egui::pos2(10.0, 10.0))
                .frame(egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(30, 30, 40, 220)))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Search:");
                        let sr = ui.text_edit_singleline(&mut self.search_query);
                        if sr.changed() {
                            self.search_results = keyword_search(&self.layout, &self.search_query);
                        }
                    });

                    ui.horizontal(|ui| {
                        if ui.selectable_label(self.area_select_active, "Area Select").clicked() {
                            self.area_select_active = !self.area_select_active;
                            self.area_select_origin = None;
                            self.area_select_radius = 0.0;
                            self.area_highlighted.clear();
                        }
                        if self.focus_sphere.is_some() {
                            if ui.small_button("Close Focus").clicked() {
                                self.focus_sphere = None;
                            }
                        }
                    });

                    ui.separator();
                    if ui.selectable_label(self.color_mode == ColorMode::Layer, "Layer").clicked() {
                        self.color_mode = ColorMode::Layer;
                    }
                    if ui.selectable_label(self.color_mode == ColorMode::IoSurface, "IO").clicked() {
                        self.color_mode = ColorMode::IoSurface;
                    }

                    ui.separator();
                    ui.label(format!("{} nodes | {} edges",
                        self.layout.total_nodes, self.layout.total_edges));

                    // Search results
                    if !self.search_results.is_empty() {
                        ui.separator();
                        for id in self.search_results.clone().into_iter().take(8) {
                            let short = id.rsplit("::").next().unwrap_or(&id);
                            if ui.small_button(short).clicked() {
                                self.selected_node = Some(id.clone());
                                self.search_query.clear();
                                self.search_results.clear();
                            }
                        }
                    }
                });
        }

        // Auto-hide toolbar
        if self.toolbar_timer > 0.0 {
            self.toolbar_timer -= ctx.input(|i| i.unstable_dt);
            if self.toolbar_timer <= 0.0 && !mouse_near_top {
                self.toolbar_visible = false;
            }
        }

        // Biography panel
        if let Some(ref sel_id) = self.selected_node.clone() {
            egui::SidePanel::right("bio").min_width(300.0).max_width(450.0).show(ctx, |ui| {
                if ui.button("Close").clicked() {
                    self.selected_node = None;
                    return;
                }
                ui.separator();
                draw_biography(ui, &self.layout, sel_id, &mut self.selected_node);
            });
        }

        // Main canvas
        egui::CentralPanel::default().show(ctx, |ui| {
            let resp = ui.allocate_rect(ui.available_rect_before_wrap(), egui::Sense::click_and_drag());
            let painter = ui.painter();
            let clip = painter.clip_rect();
            let mouse = ui.input(|i| i.pointer.hover_pos());
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);

            self.hovered_node_id = None;

            let has_focus = self.focus_sphere.is_some();
            // Split: sphere on left, focus on right (or full width if no focus)
            let sphere_w = if has_focus { clip.width() * 0.5 } else { clip.width() };
            let screen_h = clip.height();
            let sphere_ox = clip.min.x;
            let sphere_oy = clip.min.y;

            // --- Input: orbit vs area selection ---
            if self.area_select_active {
                if resp.drag_started() {
                    if let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
                        self.area_select_origin = Some(pos);
                        self.area_select_radius = 0.0;
                    }
                }
                if resp.dragged_by(egui::PointerButton::Primary) {
                    if let (Some(origin), Some(current)) = (self.area_select_origin, mouse) {
                        let dx = current.x - origin.x;
                        let dy = current.y - origin.y;
                        self.area_select_radius = (dx * dx + dy * dy).sqrt();
                    }
                }
                if resp.drag_stopped() && self.area_select_origin.is_some() {
                    if !self.area_highlighted.is_empty() {
                        let core = grepvec::canvas::sphere_view::filter_to_core(
                            &self.area_highlighted, &self.sphere_layout);
                        if !core.is_empty() {
                            let focus = grepvec::canvas::sphere_view::build_focus_sphere(
                                &core, &self.sphere_layout);
                            self.focus_camera = Camera3D::new();
                            self.focus_camera.distance = focus.bounding_radius * 2.5;
                            self.focus_sphere = Some(focus);
                        }
                    }
                    self.area_select_active = false;
                    self.area_select_origin = None;
                    self.area_select_radius = 0.0;
                    self.area_highlighted.clear();
                }
            } else {
                if resp.dragged_by(egui::PointerButton::Primary) {
                    let delta = resp.drag_delta();
                    self.sphere_camera.yaw += delta.x * 0.005;
                    self.sphere_camera.pitch = (self.sphere_camera.pitch + delta.y * 0.005)
                        .clamp(-1.4, 1.4);
                }
                if resp.secondary_clicked() && self.focus_sphere.is_some() {
                    self.focus_sphere = None;
                }
            }

            // Scroll: zoom main sphere or focus sphere based on mouse position
            let mouse_in_focus = has_focus && mouse.map(|m| m.x > sphere_ox + sphere_w).unwrap_or(false);
            if scroll != 0.0 {
                if mouse_in_focus {
                    self.focus_camera.distance = (self.focus_camera.distance - scroll * 0.5)
                        .clamp(50.0, 5000.0);
                } else {
                    self.sphere_camera.distance = (self.sphere_camera.distance - scroll * 0.5)
                        .clamp(50.0, 5000.0);
                }
            }
            ctx.request_repaint();

            // === MAIN SPHERE ===
            {
                let sphere = &self.sphere_layout;
                let fov_scale = 1.0 / (self.sphere_camera.fov / 2.0).tan();
                let half_h = screen_h / 2.0;

                // Collect focus node IDs for highlight in main sphere
                let board_ids: std::collections::HashSet<&str> = self.focus_sphere.as_ref()
                    .map(|fs| fs.nodes.iter().map(|n| n.id.as_str()).collect())
                    .unwrap_or_default();

                // Project all nodes
                struct Proj { sx: f32, sy: f32, depth: f32, pr: f32, idx: usize }
                let mut projs: Vec<Proj> = Vec::new();

                for (i, sn) in sphere.nodes.iter().enumerate() {
                    let p = Vec3 { x: sn.pos.x, y: sn.pos.y, z: sn.pos.z };
                    if let Some((lx, ly, depth)) = self.sphere_camera.project(p, sphere_w, screen_h) {
                        let sx = sphere_ox + lx;
                        let sy = sphere_oy + ly;
                        let pr = (sn.radius * fov_scale / depth * half_h).clamp(1.5, 50.0);
                        projs.push(Proj { sx, sy, depth, pr, idx: i });
                    }
                }

                // Update area selection highlights
                if self.area_select_active {
                    if let Some(origin) = self.area_select_origin {
                        self.area_highlighted.clear();
                        let sel_r = self.area_select_radius;
                        for np in &projs {
                            let dx = np.sx - origin.x;
                            let dy = np.sy - origin.y;
                            if (dx * dx + dy * dy).sqrt() <= sel_r {
                                self.area_highlighted.push(np.idx);
                            }
                        }
                    }
                }

                // Draw edges
                {
                    let mut edge_draws: Vec<(f32, f32, f32, f32, f32, bool)> = Vec::new();
                    for se in &sphere.edges {
                        let sp = &sphere.nodes[se.source_idx];
                        let tp = &sphere.nodes[se.target_idx];
                        let s3 = Vec3 { x: sp.pos.x, y: sp.pos.y, z: sp.pos.z };
                        let t3 = Vec3 { x: tp.pos.x, y: tp.pos.y, z: tp.pos.z };
                        if let (Some((lx1, ly1, sz)), Some((lx2, ly2, tz))) = (
                            self.sphere_camera.project(s3, sphere_w, screen_h),
                            self.sphere_camera.project(t3, sphere_w, screen_h),
                        ) {
                            let avg_depth = (sz + tz) * 0.5;
                            let is_sel = self.selected_node.as_ref()
                                .map(|s| s == &sp.id || s == &tp.id)
                                .unwrap_or(false);
                            edge_draws.push((
                                sphere_ox + lx1, sphere_oy + ly1,
                                sphere_ox + lx2, sphere_oy + ly2,
                                avg_depth, is_sel));
                        }
                    }
                    edge_draws.sort_by(|a, b| b.4.total_cmp(&a.4));

                    for &(sx, sy, tx, ty, depth, is_sel) in &edge_draws {
                        if is_sel {
                            // Energy flow: animated brightness pulse along selected edges
                            let edge_len = ((tx - sx).powi(2) + (ty - sy).powi(2)).sqrt();
                            let pulse_pos = (self.anim_time * 0.35).fract(); // 0..1 position along edge
                            let bright_len = 0.15; // 15% of edge length is bright

                            // Draw base edge
                            painter.line_segment(
                                [egui::pos2(sx, sy), egui::pos2(tx, ty)],
                                egui::Stroke::new(1.2, egui::Color32::from_rgba_premultiplied(60, 150, 220, 60)),
                            );

                            // Draw bright pulse segment
                            if edge_len > 5.0 {
                                let p0 = (pulse_pos - bright_len * 0.5).clamp(0.0, 1.0);
                                let p1 = (pulse_pos + bright_len * 0.5).clamp(0.0, 1.0);
                                let bx0 = sx + (tx - sx) * p0;
                                let by0 = sy + (ty - sy) * p0;
                                let bx1 = sx + (tx - sx) * p1;
                                let by1 = sy + (ty - sy) * p1;
                                painter.line_segment(
                                    [egui::pos2(bx0, by0), egui::pos2(bx1, by1)],
                                    egui::Stroke::new(2.0, egui::Color32::from_rgba_premultiplied(100, 220, 255, 180)),
                                );
                            }
                        } else {
                            let a = (18.0 - depth * 0.008).clamp(4.0, 18.0) as u8;
                            painter.line_segment(
                                [egui::pos2(sx, sy), egui::pos2(tx, ty)],
                                egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(100, 110, 140, a)),
                            );
                        }
                    }
                }

                // Draw spheres (back to front)
                projs.sort_by(|a, b| b.depth.total_cmp(&a.depth));

                for np in &projs {
                    let sn = &sphere.nodes[np.idx];
                    let r = np.pr;
                    let center = egui::pos2(np.sx, np.sy);

                    let depth_norm = ((np.depth - 50.0) / 2000.0).clamp(0.0, 1.0);
                    let alpha = (255.0 * (1.0 - depth_norm * 0.7)) as u8;

                    let is_hovered = mouse.map(|m| {
                        let dx = m.x - np.sx;
                        let dy = m.y - np.sy;
                        (dx * dx + dy * dy).sqrt() < r + 3.0
                    }).unwrap_or(false);
                    let is_selected = self.selected_node.as_ref() == Some(&sn.id);
                    let in_board = board_ids.contains(sn.id.as_str());

                    if is_hovered {
                        self.hovered_node_id = Some(sn.id.clone());
                    }

                    // Color: layer palette or IO surface classification
                    let [br, bg, bb] = if self.color_mode == ColorMode::IoSurface {
                        let name = self.layout.nodes.get(&sn.id)
                            .map(|n| n.qualified_name.as_str()).unwrap_or("");
                        io_surface_color(name)
                    } else {
                        sn.color
                    };
                    let in_area = self.area_highlighted.contains(&np.idx);
                    let item_type = self.layout.nodes.get(&sn.id)
                        .map(|n| n.item_type.as_str()).unwrap_or("function");

                    // Area selection or board membership ring
                    if in_area {
                        painter.circle_stroke(center, r + 3.0,
                            egui::Stroke::new(1.5, egui::Color32::from_rgba_premultiplied(80, 200, 255, 160)));
                    } else if in_board {
                        painter.circle_stroke(center, r + 2.0,
                            egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(255, 200, 80, 120)));
                    }

                    let fill = egui::Color32::from_rgba_premultiplied(br, bg, bb, alpha);

                    if is_selected {
                        painter.circle_filled(center, r + 2.0,
                            egui::Color32::from_rgba_premultiplied(80, 220, 255, alpha));
                        draw_node_shape(painter, center, r, item_type, fill, alpha);
                    } else if is_hovered {
                        painter.circle_filled(center, r + 1.5,
                            egui::Color32::from_rgba_premultiplied(200, 210, 220, alpha / 2));
                        let shadow_fill = egui::Color32::from_rgba_premultiplied(
                            (br as f32 * 0.5) as u8, (bg as f32 * 0.5) as u8, (bb as f32 * 0.5) as u8, alpha);
                        draw_node_shape(painter, center, r, item_type, shadow_fill, alpha);
                        draw_node_shape(painter, center, r * 0.85, item_type, fill, alpha);
                    } else {
                        let shadow_fill = egui::Color32::from_rgba_premultiplied(
                            (br as f32 * 0.45) as u8, (bg as f32 * 0.45) as u8, (bb as f32 * 0.45) as u8, alpha);
                        draw_node_shape(painter, center, r, item_type, shadow_fill, alpha);
                        draw_node_shape(painter, center, r * 0.85, item_type, fill, alpha);
                    }

                    if is_hovered && resp.clicked() {
                        self.selected_node = Some(sn.id.clone());
                    }
                }
            }

            // Selection circle overlay
            if self.area_select_active {
                if let Some(origin) = self.area_select_origin {
                    let r = self.area_select_radius;
                    if r > 2.0 {
                        painter.circle_filled(origin, r,
                            egui::Color32::from_rgba_premultiplied(60, 160, 255, 20));
                        painter.circle_stroke(origin, r,
                            egui::Stroke::new(1.5, egui::Color32::from_rgba_premultiplied(80, 200, 255, 140)));
                        let count = self.area_highlighted.len();
                        if count > 0 {
                            painter.text(
                                egui::pos2(origin.x, origin.y - r - 12.0),
                                egui::Align2::CENTER_BOTTOM,
                                format!("{} nodes", count),
                                egui::FontId::proportional(12.0),
                                egui::Color32::from_rgb(180, 220, 255));
                        }
                    }
                }
            }

            // Tooltip
            if let Some(ref hid) = self.hovered_node_id.clone() {
                if let Some(node) = self.layout.nodes.get(hid) {
                    egui::show_tooltip_at_pointer(ctx,
                        egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("stt")),
                        egui::Id::new("sntt"),
                        |ui: &mut egui::Ui| {
                            ui.label(egui::RichText::new(&node.qualified_name).strong().monospace());
                            ui.label(format!("{} | {} | {} LOC | Layer {}",
                                node.item_type, node.visibility, node.loc, node.layer));
                        });
                }
            }

            // === RIGHT HALF: Focus sphere ===
            if let Some(ref focus) = self.focus_sphere {
                let divider_x = sphere_ox + sphere_w;
                let focus_x = divider_x + 1.0;
                let focus_top = clip.min.y;
                let focus_bottom = clip.max.y;
                let focus_w = clip.max.x - focus_x;
                let focus_h = focus_bottom - focus_top;

                // Divider
                painter.line_segment(
                    [egui::pos2(divider_x, focus_top), egui::pos2(divider_x, focus_bottom)],
                    egui::Stroke::new(1.0, egui::Color32::from_gray(50)));

                // Orbit the focus camera when dragging in the right half
                if mouse_in_focus && resp.dragged_by(egui::PointerButton::Primary) && !self.area_select_active {
                    let delta = resp.drag_delta();
                    self.focus_camera.yaw += delta.x * 0.005;
                    self.focus_camera.pitch = (self.focus_camera.pitch + delta.y * 0.005)
                        .clamp(-1.4, 1.4);
                } else if !mouse_in_focus || !resp.dragged_by(egui::PointerButton::Primary) {
                    // Idle rotation
                    self.focus_camera.yaw += 0.002;
                }

                let fov_scale = 1.0 / (self.focus_camera.fov / 2.0).tan();
                let half_h = focus_h / 2.0;

                // Project focus nodes
                struct FProj { sx: f32, sy: f32, depth: f32, pr: f32, idx: usize }
                let mut fprojs: Vec<FProj> = Vec::new();

                for (i, sn) in focus.nodes.iter().enumerate() {
                    let p = Vec3 { x: sn.pos.x, y: sn.pos.y, z: sn.pos.z };
                    if let Some((lx, ly, depth)) = self.focus_camera.project(p, focus_w, focus_h) {
                        let sx = focus_x + lx;
                        let sy = focus_top + ly;
                        let pr = (sn.radius * fov_scale / depth * half_h).clamp(2.0, 60.0);
                        fprojs.push(FProj { sx, sy, depth, pr, idx: i });
                    }
                }

                // Draw edges
                for se in &focus.edges {
                    let sp = &focus.nodes[se.source_idx];
                    let tp = &focus.nodes[se.target_idx];
                    let s3 = Vec3 { x: sp.pos.x, y: sp.pos.y, z: sp.pos.z };
                    let t3 = Vec3 { x: tp.pos.x, y: tp.pos.y, z: tp.pos.z };
                    if let (Some((lx1, ly1, sz)), Some((lx2, ly2, tz))) = (
                        self.focus_camera.project(s3, focus_w, focus_h),
                        self.focus_camera.project(t3, focus_w, focus_h),
                    ) {
                        let avg_depth = (sz + tz) * 0.5;
                        let is_sel = self.selected_node.as_ref()
                            .map(|s| s == &sp.id || s == &tp.id).unwrap_or(false);
                        let (color, width) = if is_sel {
                            (egui::Color32::from_rgba_premultiplied(80, 200, 255, 120), 1.2)
                        } else {
                            let a = (25.0 - avg_depth * 0.008).clamp(5.0, 25.0) as u8;
                            (egui::Color32::from_rgba_premultiplied(100, 120, 160, a), 0.6)
                        };
                        painter.line_segment(
                            [egui::pos2(focus_x + lx1, focus_top + ly1),
                             egui::pos2(focus_x + lx2, focus_top + ly2)],
                            egui::Stroke::new(width, color));
                    }
                }

                // Draw spheres (back to front)
                fprojs.sort_by(|a, b| b.depth.total_cmp(&a.depth));

                for fp in &fprojs {
                    let sn = &focus.nodes[fp.idx];
                    let r = fp.pr;
                    let center = egui::pos2(fp.sx, fp.sy);

                    let depth_norm = ((fp.depth - 30.0) / 1500.0).clamp(0.0, 1.0);
                    let alpha = (255.0 * (1.0 - depth_norm * 0.6)) as u8;

                    let is_hovered = mouse.map(|m| {
                        let dx = m.x - fp.sx;
                        let dy = m.y - fp.sy;
                        (dx * dx + dy * dy).sqrt() < r + 3.0
                    }).unwrap_or(false);
                    let is_selected = self.selected_node.as_ref() == Some(&sn.id);

                    if is_hovered {
                        self.hovered_node_id = Some(sn.id.clone());
                    }

                    // Color: respect color mode (same as main sphere)
                    let [br, bg, bb] = if self.color_mode == ColorMode::IoSurface {
                        let name = self.layout.nodes.get(&sn.id)
                            .map(|n| n.qualified_name.as_str()).unwrap_or("");
                        io_surface_color(name)
                    } else {
                        sn.color
                    };
                    let item_type = self.layout.nodes.get(&sn.id)
                        .map(|n| n.item_type.as_str()).unwrap_or("function");
                    let fill = egui::Color32::from_rgba_premultiplied(br, bg, bb, alpha);

                    if is_selected {
                        painter.circle_filled(center, r + 2.0,
                            egui::Color32::from_rgba_premultiplied(80, 220, 255, alpha));
                        draw_node_shape(painter, center, r, item_type, fill, alpha);
                    } else if is_hovered {
                        painter.circle_filled(center, r + 1.5,
                            egui::Color32::from_rgba_premultiplied(200, 210, 220, alpha / 2));
                        let shadow_fill = egui::Color32::from_rgba_premultiplied(
                            (br as f32 * 0.5) as u8, (bg as f32 * 0.5) as u8, (bb as f32 * 0.5) as u8, alpha);
                        draw_node_shape(painter, center, r, item_type, shadow_fill, alpha);
                        draw_node_shape(painter, center, r * 0.85, item_type, fill, alpha);
                    } else {
                        let shadow_fill = egui::Color32::from_rgba_premultiplied(
                            (br as f32 * 0.45) as u8, (bg as f32 * 0.45) as u8, (bb as f32 * 0.45) as u8, alpha);
                        draw_node_shape(painter, center, r, item_type, shadow_fill, alpha);
                        draw_node_shape(painter, center, r * 0.85, item_type, fill, alpha);
                    }

                    // Labels (shown at larger projected size since fewer nodes)
                    if r > 8.0 {
                        let name = self.layout.nodes.get(&sn.id)
                            .map(|n| n.name.as_str()).unwrap_or("?");
                        let font_sz = (r * 0.6).clamp(7.0, 13.0);
                        let max_chars = ((r * 2.0 / (font_sz * 0.55)) as usize).max(3);
                        let label = if name.len() > max_chars { &name[..max_chars] } else { name };
                        painter.text(
                            egui::pos2(fp.sx, fp.sy + r + font_sz * 0.6),
                            egui::Align2::CENTER_TOP,
                            label, egui::FontId::monospace(font_sz),
                            egui::Color32::from_rgba_premultiplied(220, 220, 240, alpha));
                    }

                    if is_hovered && resp.clicked() {
                        self.selected_node = Some(sn.id.clone());
                    }
                }

                // Title overlay
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(focus_x, focus_top),
                        egui::vec2(focus_w, 24.0)),
                    0.0, egui::Color32::from_rgba_premultiplied(16, 18, 26, 220));
                painter.text(
                    egui::pos2(focus_x + 10.0, focus_top + 5.0),
                    egui::Align2::LEFT_TOP,
                    format!("{} nodes | {} edges", focus.nodes.len(), focus.edges.len()),
                    egui::FontId::proportional(11.0),
                    egui::Color32::from_rgb(170, 195, 225));
            }

            // Quit confirmation overlay
            if self.quit_pending {
                let screen_center = egui::pos2(clip.center().x, clip.max.y - 40.0);
                painter.rect_filled(
                    egui::Rect::from_center_size(screen_center, egui::vec2(280.0, 28.0)),
                    6.0, egui::Color32::from_rgba_premultiplied(20, 20, 30, 230));
                painter.text(
                    screen_center,
                    egui::Align2::CENTER_CENTER,
                    "Press Escape again to quit",
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(255, 180, 80));
            }
        });
    }
}

fn draw_biography(
    ui: &mut egui::Ui,
    layout: &CircuitLayout,
    node_id: &str,
    selected: &mut Option<String>,
) {
    let node = match layout.nodes.get(node_id) {
        Some(n) => n,
        None => { ui.label("Node not found"); return; }
    };

    ui.heading(egui::RichText::new(&node.name).monospace());
    ui.label(egui::RichText::new(&node.qualified_name).small().weak().monospace());
    ui.label(format!("{} | {} | {} LOC", node.item_type, node.visibility, node.loc));
    if !node.file_path.is_empty() {
        ui.label(egui::RichText::new(format!("{}:{}", node.file_path, node.line_start)).small().monospace());
    }
    ui.separator();

    // Which trees contain this node?
    let in_trees: Vec<(usize, &str)> = layout.endpoint_trees.iter().enumerate()
        .filter(|(_, t)| t.nodes.iter().any(|tn| tn.node_id == node_id))
        .map(|(i, t)| (i, t.endpoint_name.as_str()))
        .collect();

    if in_trees.len() > 1 {
        ui.label(egui::RichText::new(format!("Appears in {} endpoint circuits:", in_trees.len())).strong());
        for (_ti, name) in &in_trees {
            ui.label(format!("  {}", name));
        }
        ui.separator();
    }

    // Callers
    let callers: Vec<&str> = layout.edges.iter()
        .filter(|e| e.target_id == node_id && e.edge_type == "calls")
        .map(|e| e.source_id.as_str())
        .collect();
    if !callers.is_empty() {
        ui.label(egui::RichText::new(format!("Called by ({})", callers.len())).strong());
        for c in &callers {
            let short = c.rsplit("::").next().unwrap_or(c);
            if ui.link(format!("<- {}", short)).clicked() {
                *selected = Some(c.to_string());
            }
        }
    }

    // Callees
    let callees: Vec<&str> = layout.edges.iter()
        .filter(|e| e.source_id == node_id && e.edge_type == "calls")
        .map(|e| e.target_id.as_str())
        .collect();
    if !callees.is_empty() {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(format!("Calls ({})", callees.len())).strong());
        for c in &callees {
            let short = c.rsplit("::").next().unwrap_or(c);
            if ui.link(format!("-> {}", short)).clicked() {
                *selected = Some(c.to_string());
            }
        }
    }

    // Boundary deps
    let boundary: Vec<&str> = layout.edges.iter()
        .filter(|e| e.source_id == node_id && e.target_id.starts_with("boundary::"))
        .map(|e| e.target_id.as_str())
        .collect();
    if !boundary.is_empty() {
        ui.add_space(6.0);
        ui.label(egui::RichText::new("External Dependencies").strong());
        for dep in &boundary {
            let name = dep.strip_prefix("boundary::").unwrap_or(dep);
            ui.label(format!("  * {}", name));
        }
    }

    // Source code
    if !node.file_path.is_empty() && node.line_start > 0 {
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new(format!("Source ({}:{}-{})",
            node.file_path, node.line_start, node.line_start + node.loc)).strong());

        // Resolve full path: try known base directories
        let base_paths = [
            format!("/home/christopher/enscribe-io/{}", node.repo),
            format!("/home/christopher/enscribe-io"),
        ];

        let mut source_loaded = false;
        for base in &base_paths {
            let full_path = format!("{}/{}", base, node.file_path);
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let lines: Vec<&str> = content.lines().collect();
                let start = (node.line_start as usize).saturating_sub(1);
                let end = (start + node.loc as usize).min(lines.len());

                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for (i, line) in lines[start..end].iter().enumerate() {
                        let line_num = start + i + 1;
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{:4}", line_num))
                                .monospace().weak().small());
                            ui.label(egui::RichText::new(*line)
                                .monospace().small());
                        });
                    }
                });
                source_loaded = true;
                break;
            }
        }

        if !source_loaded {
            ui.label(egui::RichText::new("(source file not found on disk)").weak().italics());
        }
    }
}

fn keyword_search(layout: &CircuitLayout, query: &str) -> Vec<String> {
    if query.len() < 2 { return Vec::new(); }
    let stop = ["where","do","we","how","does","is","the","a","an","in","on","to","for","of","and","or","what","when","who","that","this","it","handle"];
    let terms: Vec<String> = query.to_lowercase().split_whitespace()
        .filter(|w| w.len() >= 2 && !stop.contains(w))
        .map(|w| w.to_string()).collect();
    if terms.is_empty() { return Vec::new(); }

    let mut results: Vec<(String, usize)> = layout.nodes.iter()
        .map(|(id, n)| {
            let hay = format!("{} {} {} {}", id, n.name, n.module_path, n.item_type).to_lowercase();
            let hits = terms.iter().filter(|t| hay.contains(t.as_str())).count();
            (id.clone(), hits)
        })
        .filter(|(_, h)| *h > 0)
        .collect();
    results.sort_by(|a, b| b.1.cmp(&a.1));
    results.into_iter().take(20).map(|(id, _)| id).collect()
}

/// IO surface color by name pattern (from classify.rs logic).
fn io_surface_color(name: &str) -> [u8; 3] {
    let n = name.to_lowercase();
    if ["auth","hmac","login","jwt","token","oauth","permission"].iter().any(|k| n.contains(k)) {
        [220, 60, 60]    // Identity — red
    } else if ["page","component","template","render","dashboard","canvas"].iter().any(|k| n.contains(k)) {
        [100, 180, 220]  // View — light blue
    } else if ["stream","sse","websocket","subscribe","push"].iter().any(|k| n.contains(k)) {
        [180, 100, 220]  // Stream — purple
    } else if ["health","backup","metrics","admin","debug"].iter().any(|k| n.contains(k)) {
        [160, 160, 160]  // Operate — gray
    } else if ["ingest","import","upload","webhook","bulk"].iter().any(|k| n.contains(k)) {
        [220, 160, 60]   // Ingest — orange
    } else if ["cron","schedule","job","worker","cleanup"].iter().any(|k| n.contains(k)) {
        [160, 120, 80]   // Schedule — brown
    } else if ["create","update","delete","set_","add_","write"].iter().any(|k| n.contains(k)) {
        [80, 200, 120]   // Action — green
    } else if ["search","get_","list_","fetch","stats","export"].iter().any(|k| n.contains(k)) {
        [80, 160, 220]   // Query — blue
    } else {
        [90, 100, 120]   // Internal — muted
    }
}

/// Draw a shape for the given item_type. Falls back to circle for small radius.
fn draw_node_shape(
    painter: &egui::Painter,
    center: egui::Pos2,
    r: f32,
    item_type: &str,
    fill: egui::Color32,
    alpha: u8,
) {
    if r < 8.0 {
        // Too small for shape detail — just a circle
        painter.circle_filled(center, r, fill);
        return;
    }
    match item_type {
        "struct" => {
            // Rounded rectangle
            let rect = egui::Rect::from_center_size(center, egui::vec2(r * 1.8, r * 1.4));
            painter.rect_filled(rect, r * 0.3, fill);
        }
        "enum" => {
            // Diamond
            let pts = vec![
                egui::pos2(center.x, center.y - r),
                egui::pos2(center.x + r, center.y),
                egui::pos2(center.x, center.y + r),
                egui::pos2(center.x - r, center.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, fill, egui::Stroke::NONE));
        }
        "impl" => {
            // Hexagon
            let pts: Vec<egui::Pos2> = (0..6).map(|i| {
                let angle = std::f32::consts::FRAC_PI_3 * i as f32 - std::f32::consts::FRAC_PI_6;
                egui::pos2(center.x + r * angle.cos(), center.y + r * angle.sin())
            }).collect();
            painter.add(egui::Shape::convex_polygon(pts, fill, egui::Stroke::NONE));
        }
        "trait" => {
            // Circle with inner ring (distinguishing mark)
            painter.circle_filled(center, r, fill);
            painter.circle_stroke(center, r * 0.6,
                egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, alpha / 3)));
        }
        _ => {
            // Function / default — circle
            painter.circle_filled(center, r, fill);
        }
    }
}
