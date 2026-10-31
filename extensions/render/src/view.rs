//! The 3D view of a model, as Blender's: the middle button orbits, with Shift pans and with Ctrl zooms, as the wheel does, and Alt lends the primary button the same; the number keys look
//! along the axes (1 the front, 3 the right, 7 the top, with Shift the opposite side), 5 switches the projection, Home shows everything and the period what is selected. A click selects the part it lands on, which is the selection everything else shows, and the handles on it move what
//! holds its transform: the inspector follows while they do, and letting go writes it as one step. A part with nowhere to keep one has no handles.

use pe2_ext::{Asset, Image, Style, Texture, Ui, View, ViewportInput};

use pe2_ext::Value;

use crate::assembly::{self, Assembled, Place};
use crate::camera::Camera;
use crate::gizmo::{self, AXES, Ball, Handle, Handles, Overlay};
use crate::math::{M3, V3, add, compose, m4_bytes, scale, sub};
use crate::parts::{self, BONE};
use crate::skeleton;
use crate::render::{Drawn, Renderer};
use crate::material;
use crate::prefab;
use crate::scene::{self, BlendedFace, Paint, Scene, upright};

/// How the faces are shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shading {
    Solid,
    Wire,
}

/// What is shown besides the model.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Shown {
    grid: bool,
    bones: bool,
    normals: bool,
}

/// What a view is told to draw again by.
#[derive(Default)]
struct Dirty {
    /// The faces moved or changed color.
    geometry: bool,
    /// The lines in the world, and the floor.
    lines: bool,
    /// The view did.
    frame: bool,
}

/// What the pointer is doing with the ball of axes.
#[derive(Clone, Copy, Default)]
struct BallState {
    turning: bool,
    over: bool,
}

pub struct ModelView {
    scene: Option<Result<Scene, String>>,
    renderer: Option<Renderer>,
    /// Pixels waiting for the renderer.
    atlas: Option<((u32, u32), Vec<u8>)>,
    /// By part, where its transform is written.
    places: Vec<Option<Place>>,
    /// The part a handle let go of.
    released: Option<usize>,
    /// Whether what a handle does can be written.
    editable: bool,
    /// Only what is selected shows.
    isolated: bool,
    /// What faces are painted by.
    paint: Paint,
    /// What blends, and the eye it was last put in order for.
    blended: Vec<BlendedFace>,
    sorted_for: Option<V3>,
    camera: Camera,
    shading: Shading,
    shown: Shown,
    selected: Option<usize>,
    dirty: Dirty,
    /// What the pointer holds.
    held: Option<Handle>,
    hovering: Option<Handle>,
    last_pointer: Option<(f32, f32)>,
    ball_state: BallState,
}

impl Default for ModelView {
    fn default() -> ModelView {
        ModelView {
            scene: None,
            renderer: None,
            atlas: None,
            places: Vec::new(),
            released: None,
            editable: false,
            isolated: false,
            paint: Paint::Off,
            blended: Vec::new(),
            sorted_for: None,
            camera: Camera::default(),
            shading: Shading::Solid,
            shown: Shown { grid: true, bones: true, normals: false },
            selected: None,
            dirty: Dirty { geometry: true, lines: true, frame: true },
            held: None,
            hovering: None,
            last_pointer: None,
            ball_state: BallState::default(),
        }
    }
}

fn pressed_primary(input: &ViewportInput) -> bool {
    input.pressed.0
}

impl ModelView {
    fn load(&mut self, ui: &Ui) {
        if self.scene.is_some() {
            return;
        }
        let Some(asset) = ui.asset() else { return };
        let scene = assembly::assemble(ui.project(), &asset).map_err(|e| format!("{e:#}")).map(|assembled| {
            self.places = assembled.nodes.iter().map(|node| node.place.clone()).collect();
            self.pictured(ui, assembled)
        });
        // An edit makes the view again: what it looked at, and how, stay.
        let kept = ui.note().and_then(|note| self.resume(&note));
        if let (None, Ok(scene)) = (kept, &scene) {
            self.camera.frame(scene.center, scene.radius);
        }
        self.scene = Some(scene);
    }

    /// How this was being looked at, from what `note` left.
    fn resume(&mut self, note: &str) -> Option<()> {
        let (flags, rest) = note.split_once(' ')?;
        let (paints, camera) = rest.split_once(' ')?;
        let mut flags = flags.chars().map(|flag| flag == '1');
        self.shading = if flags.next()? { Shading::Solid } else { Shading::Wire };
        self.shown = Shown { grid: flags.next()?, bones: flags.next()?, normals: flags.next()? };
        self.isolated = flags.next()?;
        self.paint = Paint::ALL.into_iter().find(|(paint, _)| paint.written() == paints.chars().next()).map_or(Paint::Off, |(paint, _)| paint);
        self.camera = Camera::resumed(camera)?;
        Some(())
    }

    fn note(&self) -> String {
        let flag = |on: bool| if on { '1' } else { '0' };
        let flags: String = [self.shading == Shading::Solid, self.shown.grid, self.shown.bones, self.shown.normals, self.isolated].map(flag).iter().collect();
        format!("{flags} {} {}", self.paint.written().unwrap_or('-'), self.camera.written())
    }

    /// Show what the inspector is being dragged to, before it is written: as `held_now` in reverse.
    fn follow(&mut self, ui: &Ui) {
        let (Some((key, value)), Some(part)) = (ui.editing(), self.selected) else { return };
        let Some(numbers) = skeleton::numbers_of(&table_of(&key, value), &key) else { return };
        #[expect(clippy::cast_possible_truncation, reason = "what a coordinate or an angle comes to")]
        let [x, y, z] = numbers.map(|number| number as f32);
        let held = self.places.get(part).and_then(Option::as_ref);
        let child = ui.asset().and_then(|asset| match held {
            Some(Place::Fill(within)) => Some(prefab::fill_at(&asset, within).cloned().unwrap_or(Value::Table(pe2_ext::Table::new()))),
            _ => None,
        });
        let Some(Ok(scene)) = &mut self.scene else { return };
        let (Some(rest), Some(pose)) = (scene.rest.get(part).copied(), scene.pose.get_mut(part)) else { return };
        let turned = |degrees: [f32; 3]| skeleton::matrix(degrees.map(f64::from)).map(|row| row.map(|x| f32::from(x) / 4096.0));
        match (key.as_str(), &child) {
            ("translation", None) => pose.translation = [x, y, z],
            ("rotation", None) => pose.rotation = turned([x, y, z]),
            ("translation", Some(child)) => {
                #[expect(clippy::cast_precision_loss, reason = "a coordinate is far from where floats lose it")]
                let was = skeleton::translation_of(child).unwrap_or_default().map(|x| x as f32);
                pose.translation = add(rest.translation, sub([x, y, z], was));
            }
            ("rotation", Some(child)) => {
                #[expect(clippy::cast_possible_truncation, reason = "an angle")]
                let was = turned(skeleton::numbers_of(child, "rotation").unwrap_or_default().map(|x| x as f32));
                if let Some(back) = crate::math::inverse(&was) {
                    pose.rotation = compose(&compose(&turned([x, y, z]), &back), &rest.rotation);
                }
            }
            _ => return,
        }
        self.dirty.geometry = true;
        self.dirty.lines = true;
    }

    /// Where a part now is, as its place holds it: its translation and its rotation.
    fn held_now(&self, asset: &Asset, part: usize) -> Option<(Value, Value)> {
        let Some(Ok(scene)) = &self.scene else { return None };
        let (pose, rest) = (scene.pose.get(part)?, scene.rest.get(part)?);
        #[expect(clippy::cast_possible_truncation, reason = "a coordinate is well within range")]
        let rounded = |at: V3| Value::Array(at.map(|x| Value::Integer(i64::from(x.round() as i32))).to_vec());
        // Adding zero takes the sign off what rounded to none: there is no turn of -0.0.
        let degrees = |rotation: &M3| Value::Array(parts::euler_degrees(rotation).map(|x| Value::Float(f64::from((x * 10.0).round() / 10.0 + 0.0))).to_vec());
        if let Place::Fill(within) = self.places.get(part)?.as_ref()? {
            // What fills a part is held over what its model's root says: what moved is the difference.
            let child = prefab::fill_at(asset, within)?;
            #[expect(clippy::cast_precision_loss, reason = "a coordinate is far from where floats lose it")]
            let was = skeleton::translation_of(child).unwrap_or_default().map(|x| x as f32);
            let by = sub(pose.translation, rest.translation);
            let was_turned = skeleton::matrix(skeleton::numbers_of(child, "rotation").unwrap_or_default()).map(|row| row.map(|x| f32::from(x) / 4096.0));
            let turned = crate::math::inverse(&rest.rotation).map_or(was_turned, |inverse| compose(&compose(&pose.rotation, &inverse), &was_turned));
            return Some((rounded(add(was, by)), degrees(&turned)));
        }
        Some((rounded(pose.translation), degrees(&pose.rotation)))
    }

    /// Show the inspector where the part held is.
    fn report(&self, ui: &mut Ui, part: usize) {
        let held = ui.asset().and_then(|asset| self.held_now(&asset, part));
        if let Some((translation, rotation)) = held {
            ui.live(&[("translation", translation), ("rotation", rotation)]);
        }
    }

    /// What a handle did to a part, written where the part's transform is: a step of its own, when it lets go.
    fn commit(&mut self, ui: &mut Ui, part: usize) {
        // A handle pressed and let go moved nothing, and leaves nothing written.
        let moved = matches!(&self.scene, Some(Ok(scene)) if scene.pose.get(part) != scene.rest.get(part));
        let written = ui.asset().filter(|_| moved).and_then(|asset| {
            let (translation, rotation) = self.held_now(&asset, part)?;
            let set = |table: &mut pe2_ext::Table| {
                table.insert("translation".to_string(), translation.clone());
                table.insert("rotation".to_string(), rotation.clone());
            };
            let (key, value) = match self.places.get(part)?.as_ref()? {
                Place::Joint(at) => {
                    let mut joints = asset.array("joints").to_vec();
                    set(joints.get_mut(*at)?.as_table_mut()?);
                    ("joints".to_string(), Value::Array(joints))
                }
                Place::Pose(name) => {
                    let mut poses = asset.field("pose").and_then(Value::as_table).cloned().unwrap_or_default();
                    let mut joint = pe2_ext::Table::new();
                    set(&mut joint);
                    poses.insert(name.clone(), Value::Table(joint));
                    ("pose".to_string(), Value::Table(poses))
                }
                Place::Fill(within) => prefab::edited(&asset, within, set)?,
            };
            (asset.field(&key) != Some(&value)).then_some((key, value))
        });
        if let Some((key, value)) = written {
            ui.set_step(&key, Some(&value));
        } else if let Some(Ok(scene)) = &mut self.scene {
            // Less than a whole unit: nothing to write, and so nothing moved.
            scene.reset_pose();
            self.dirty.geometry = true;
            self.dirty.lines = true;
            ui.live(&[]);
        }
    }

    /// The scene of what was assembled, with its pictures gathered in one atlas; one that cannot be read shows plain.
    fn pictured(&mut self, ui: &Ui, assembled: Assembled) -> Scene {
        let project = ui.project();
        let drawn = || assembled.slots.iter().filter_map(|slot| slot.drawn.as_ref());
        let mut seen: Vec<&assembly::Bound> = Vec::new();
        let mut images = Vec::new();
        for bound in drawn().flat_map(|drawn| [drawn.main.as_ref(), drawn.environment.as_ref()]).flatten() {
            if !seen.contains(&bound)
                && let Some(Ok(image)) = project.image(&bound.image, &bound.output())
            {
                seen.push(bound);
                images.push(image);
            }
        }
        let pictures: Vec<_> = images.iter().map(|image| (image.width, image.height, image.rgba.as_slice())).collect();
        let (width, height, pixels, regions) = scene::pack(&pictures);
        let region_of = |bound: Option<&assembly::Bound>| bound.and_then(|bound| seen.iter().position(|known| *known == bound)).map(|at| regions[at]);
        let of_slots = |pick: fn(&assembly::Drawn) -> Option<&assembly::Bound>| assembled.slots.iter().map(|slot| region_of(slot.drawn.as_ref().and_then(pick))).collect::<Vec<_>>();
        let by_material = of_slots(|drawn| drawn.main.as_ref());
        let shares = assembled.slots.iter().map(|slot| slot.drawn.as_ref().map_or(0.0, |drawn| f32::from(drawn.strength) / f32::from(material::LAYER_FULL)));
        let by_layer = of_slots(|drawn| drawn.environment.as_ref()).into_iter().zip(shares).map(|(region, share)| region.map(|region| (region, share))).collect();
        #[expect(clippy::cast_possible_truncation, reason = "an atlas is a few cells tall")]
        if !images.is_empty() {
            self.atlas = Some(((width as u32, height as u32), pixels));
        }
        let mut scene = Scene::new(assembled.model);
        scene.regions = by_material;
        scene.layers = by_layer;
        scene
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let Some(Ok(scene)) = &mut self.scene else { return };
        let mut frame_all = false;
        let (mut shading, Shown { mut grid, mut bones, mut normals }, mut orthographic) = (self.shading, self.shown, self.camera.orthographic);
        let (mut isolated, mut paint) = (self.isolated, self.paint);
        let was = (bones, orthographic);
        // A skeleton has no faces to shade, and is its bones.
        let faces = scene.model.faces.len();
        let parts = scene.model.parts.len();
        let stats = match (faces, parts) {
            (0, _) => format!("{parts} joints"),
            (_, 1) => format!("{faces} faces"),
            _ => format!("{parts} parts  ·  {faces} faces"),
        };
        ui.wrapped(|ui| {
            if faces > 0 {
                if ui.toggle("Solid", shading == Shading::Solid) {
                    shading = Shading::Solid;
                }
                if ui.toggle("Wireframe", shading == Shading::Wire) {
                    shading = Shading::Wire;
                }
                ui.styled("|", Style::Note);
            }
            grid ^= ui.toggle("Grid", grid);
            if faces > 0 && parts > 1 {
                bones ^= ui.toggle("Bones", bones);
            }
            if faces > 0 && scene.model.normals.len() > 1 {
                normals ^= ui.toggle("Normals", normals);
            }
            ui.styled("|", Style::Note);
            prefab::bar(ui);
            orthographic ^= ui.toggle("Orthographic", orthographic);
            frame_all = ui.button("Frame");
            if parts > 1 && faces > 0 && (self.selected.is_some() || isolated) {
                isolated ^= ui.toggle("Isolate", isolated);
            }
            if faces > 0 && shading == Shading::Solid {
                ui.styled("|", Style::Note);
                let options: Vec<String> = Paint::ALL.iter().map(|(_, label)| (*label).to_string()).collect();
                let shown = Paint::ALL.iter().find(|(known, _)| *known == paint).map_or("", |(_, label)| label);
                if let Some(chosen) = ui.combo("paint", shown, &options) {
                    paint = Paint::ALL[chosen].0;
                }
            }
            ui.styled(&stats, Style::Note);
        });
        // A color says something only beside what it says.
        let legend = if shading == Shading::Solid { paint.legend(&scene.model) } else { Vec::new() };
        if !legend.is_empty() {
            ui.wrapped(|ui| {
                for (color, label, count) in &legend {
                    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a color's share, of a byte")]
                    let [red, green, blue] = color.map(|x| (x * 255.0) as u8);
                    let swatch = Texture::new(format!("paint {label}"), Image { width: 1, height: 1, rgba: vec![red, green, blue, 255] });
                    ui.image(&swatch, 10.0, 10.0);
                    ui.styled(&format!("{label}  {count}"), Style::Note);
                }
            });
        }
        if paint != self.paint {
            self.paint = paint;
            self.dirty.geometry = true;
            self.dirty.frame = true;
        }
        if shading != self.shading {
            self.dirty.lines = true;
            self.dirty.geometry = true;
        }
        if grid != self.shown.grid || normals != self.shown.normals {
            self.dirty.lines = true;
        }
        if isolated != self.isolated {
            self.isolated = isolated;
            self.dirty.geometry = true;
            self.dirty.lines = true;
        }
        (self.shading, self.shown, self.camera.orthographic) = (shading, Shown { grid, bones, normals }, orthographic);
        if frame_all {
            self.camera.frame(scene.center, scene.radius);
        }
        if bones != was.0 || orthographic != was.1 || frame_all {
            self.dirty.frame = true;
        }
    }

    fn selection_from(&mut self, ui: &Ui) {
        let selected = ui.selected().as_deref().and_then(parts::bone_of);
        if selected != self.selected {
            self.selected = selected;
            self.dirty.geometry = true;
        }
    }

    fn selected_at(&self) -> Option<V3> {
        let Some(Ok(scene)) = &self.scene else { return None };
        self.selected_in(scene)
    }

    fn selected_in(&self, scene: &Scene) -> Option<V3> {
        Some(upright(scene.worlds().get(self.selected?)?.position))
    }

    /// The handles on what is selected, where a move of it can be kept: only what is written stays moved.
    fn handles_in(&self, scene: &Scene, size: (f32, f32)) -> Option<Handles> {
        let kept = self.editable && matches!(self.places.get(self.selected?), Some(Some(_)));
        kept.then(|| self.selected_in(scene)).flatten().and_then(|at| Handles::new(&self.camera, at, size))
    }

    fn keys(&mut self, input: &ViewportInput) {
        for key in &input.keys {
            // Blender's is Ctrl, which here is the editor's own: Ctrl+1 shows a document's preview.
            let sign = if input.modifiers.shift { -1.0 } else { 1.0 };
            match key.as_str() {
                "Num1" => self.camera.look_along(scale(AXES[2], sign)),
                "Num3" => self.camera.look_along(scale(AXES[0], sign)),
                "Num7" => self.camera.look_along(scale(AXES[1], sign)),
                "Num5" => self.camera.orthographic = !self.camera.orthographic,
                "Home" => {
                    if let Some(Ok(scene)) = &self.scene {
                        self.camera.frame(scene.center, scene.radius);
                    }
                }
                "Period" => {
                    if let (Some(at), Some(Ok(scene))) = (self.selected_at(), &self.scene) {
                        self.camera.frame(at, scene.radius * 0.35);
                    }
                }
                _ => continue,
            }
            self.dirty.frame = true;
        }
    }

    /// Whichever of the view's own things the pointer took.
    fn navigate(&mut self, input: &ViewportInput, size: (f32, f32)) -> bool {
        let moved = input.drag.0 != 0.0 || input.drag.1 != 0.0;
        if input.scroll != 0.0 {
            self.camera.zoom(input.scroll);
            self.dirty.frame = true;
        }
        let orbiting = input.held.2 || (input.modifiers.alt && input.held.0);
        if orbiting && moved {
            let (dx, dy) = input.drag;
            if input.modifiers.shift {
                self.camera.pan(dx, dy, size.1);
            } else if input.modifiers.ctrl {
                self.camera.zoom(dy * 2.0);
            } else {
                self.camera.orbit(dx, dy);
            }
            self.dirty.frame = true;
        }
        orbiting
    }

    fn ball(&mut self, input: &ViewportInput, ball: &Ball) -> bool {
        if input.pressed.0 && ball.holds(input.pointer) {
            match input.pointer.and_then(|at| ball.end_at(at)) {
                Some(end) => {
                    self.camera.look_along(end);
                    self.dirty.frame = true;
                }
                None => self.ball_state.turning = true,
            }
            return true;
        }
        if self.ball_state.turning {
            if input.held.0 {
                self.camera.orbit(input.drag.0, input.drag.1);
                self.dirty.frame = true;
            } else {
                self.ball_state.turning = false;
            }
            return true;
        }
        false
    }

    fn handles(&mut self, input: &ViewportInput, handles: Option<&Handles>) -> bool {
        let Some(handles) = handles else {
            self.hovering = None;
            return false;
        };
        let hovering = input.pointer.and_then(|at| handles.at(at));
        if hovering != self.hovering {
            self.hovering = hovering;
            self.dirty.frame = true;
        }
        if pressed_primary(input) && !input.modifiers.alt && self.held.is_none() {
            self.held = hovering;
        }
        if !input.held.0 && self.held.take().is_some() {
            self.released = self.selected;
        }
        let (Some(held), Some(part), Some(Ok(scene))) = (self.held, self.selected, &mut self.scene) else { return false };
        if let (Some(now), Some(before)) = (input.pointer, self.last_pointer) {
            match held {
                Handle::Move(axis) => scene.move_part(part, scale(AXES[axis], handles.along(axis, input.drag))),
                Handle::Turn(axis) => scene.turn_part(part, AXES[axis], handles.turned(AXES[axis], self.camera.axes().2, before, now)),
            }
            self.dirty.geometry = true;
            self.dirty.lines = true;
        }
        true
    }

    fn interact(&mut self, ui: &mut Ui, input: &ViewportInput, size: (f32, f32)) {
        // A pointer elsewhere asks nothing of what is shown: nothing here is worked out for it.
        if !input.hovered && self.held.is_none() && !self.ball_state.turning {
            if self.hovering.take().is_some() || self.ball_state.over {
                self.ball_state.over = false;
                self.dirty.frame = true;
            }
            return;
        }
        self.keys(input);
        let ball = Ball::new(&self.camera, size);
        let handles = self.scene.as_ref().and_then(|scene| scene.as_ref().ok()).and_then(|scene| self.handles_in(scene, size));
        // What the ball draws depends on where the pointer is over it.
        if ball.holds(input.pointer) || self.ball_state.over {
            self.dirty.frame = self.dirty.frame || input.pointer != self.last_pointer;
        }
        self.ball_state.over = ball.holds(input.pointer);
        let on_ball = self.ball(input, &ball);
        let on_handle = !on_ball && self.handles(input, handles.as_ref());
        if let Some(part) = self.released.take() {
            self.commit(ui, part);
        } else if let (Some(_), Some(part)) = (self.held, self.selected) {
            self.report(ui, part);
        }
        let navigating = self.navigate(input, size);
        if input.clicked && !on_ball && !on_handle && !navigating && let (Some(at), Some(Ok(scene))) = (input.pointer, &self.scene) {
            let (origin, direction) = self.camera.ray(at, size);
            if let Some(part) = scene.pick(origin, direction) {
                ui.select(&format!("{BONE}{part}"));
            }
        }
        self.last_pointer = input.pointer;
    }

    fn lines(&self, scene: &Scene) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut push = |a: V3, b: V3, color: [f32; 3]| {
            for at in [a, b] {
                for value in at.into_iter().chain([0.0, 1.0, 0.0]).chain(color) {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
        };
        if self.shading == Shading::Wire {
            for (a, b) in scene.edges() {
                push(a, b, [0.78, 0.82, 0.88]);
            }
        }
        if self.shown.normals {
            for (at, normal) in scene.normal_lines() {
                push(at, add(at, scale(normal, scene.radius * 0.04)), [0.35, 0.85, 0.95]);
            }
        }
        bytes
    }

    fn overlay(&self, scene: &Scene, size: (f32, f32), pointer: Option<(f32, f32)>) -> Vec<u8> {
        let mut overlay = Overlay::new(size);
        if self.shown.bones {
            gizmo::draw_bones(&mut overlay, &self.camera, &scene.bones(), self.selected, size);
        }
        if let Some(handles) = self.handles_in(scene, size) {
            handles.draw(&mut overlay, self.held.or(self.hovering));
        }
        Ball::new(&self.camera, size).draw(&mut overlay, pointer);
        overlay.bytes
    }

    fn draw(&mut self, size: (f32, f32), pointer: Option<(f32, f32)>) {
        let Some(Ok(mut scene)) = self.scene.take() else { return };
        let geometry = std::mem::take(&mut self.dirty.geometry);
        if geometry {
            scene.isolate(self.selected.filter(|_| self.isolated));
            scene.paint = self.paint;
        }
        let mut triangles = (geometry && self.shading == Shading::Solid).then(|| scene.triangles(self.selected));
        if geometry {
            self.blended = triangles.as_mut().map_or_else(Vec::new, |triangles| std::mem::take(&mut triangles.blended));
            self.sorted_for = None;
        }
        // The game sorts what blends every frame, by where each face is from the eye.
        let back = self.camera.axes().2;
        let sorted = (self.sorted_for != Some(back)).then(|| {
            self.sorted_for = Some(back);
            scene::in_depth_order(&self.blended, back)
        });
        let lines = std::mem::take(&mut self.dirty.lines) || geometry;
        let world = lines.then(|| self.lines(&scene));
        let over = self.overlay(&scene, size, pointer);
        let (camera, step) = (self.camera, gizmo::floor_step(scene.radius));
        let floor = lines.then(|| if self.shown.grid { gizmo::floor_quad(step) } else { Vec::new() });
        if let Some(renderer) = &mut self.renderer {
            if let Some((size, pixels)) = self.atlas.take() {
                renderer.set_atlas(size, &pixels);
            }
            if let Some(floor) = &floor {
                renderer.set_floor(floor);
            }
            if geometry {
                renderer.set_triangles(triangles.as_ref());
            }
            if let Some((bytes, runs)) = sorted {
                renderer.set_blended(&bytes, runs);
            }
            if let Some(world) = &world {
                renderer.set_lines(world);
            }
            renderer.set_overlay(&over);
            let back = camera.axes().2;
            let light = crate::math::normalized([back[0], back[1] + 0.35, back[2]]);
            renderer.draw(&Drawn { camera: m4_bytes(&camera.view_projection(size.0 / size.1)), light, step });
        }
        self.scene = Some(Ok(scene));
    }
}

impl View for ModelView {
    fn show(&mut self, ui: &mut Ui) {
        self.load(ui);
        match &self.scene {
            None => return,
            Some(Err(why)) => {
                ui.error(why);
                return;
            }
            Some(Ok(_)) => {}
        }
        self.selection_from(ui);
        self.editable = ui.editable();
        self.follow(ui);
        self.toolbar(ui);
        if self.renderer.is_none() {
            self.renderer = Renderer::new();
        }
        let Some(renderer) = &mut self.renderer else {
            ui.styled("Nothing here draws", Style::Note);
            return;
        };
        let (width, height, pixels) = ui.available();
        let size = (width.max(16.0), height.max(16.0));
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a window's pixels")]
        let target = ((size.0 * pixels).round() as u32, (size.1 * pixels).round() as u32);
        if renderer.resize(target) {
            self.dirty.frame = true;
        }
        let input = ui.gpu_viewport(renderer.id(), size.0, size.1);
        self.interact(ui, &input, size);
        if self.dirty.geometry || self.dirty.lines || self.dirty.frame {
            self.draw(size, input.pointer);
            self.dirty.frame = false;
            ui.leave_note(&self.note());
        }
    }
}

/// A field by itself, as what reads numbers from a table takes it.
fn table_of(key: &str, value: Value) -> Value {
    let mut table = pe2_ext::Table::new();
    table.insert(key.to_string(), value);
    Value::Table(table)
}
