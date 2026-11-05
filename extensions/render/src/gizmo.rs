//! What is drawn over a scene, in pixels: the ball in the corner that says which way is which and turns the view, the handles on what is selected, the bones. All of it is flat, always on
//! top, and known to the pixel, so what it is drawn from is what the pointer is tested against.

use crate::camera::Camera;
use crate::math::{V3, add, cross, dot, normalized, scale};

pub const AXES: [V3; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const AXIS_COLORS: [[f32; 3]; 3] = [[0.91, 0.27, 0.32], [0.46, 0.75, 0.24], [0.26, 0.55, 0.93]];
/// Bytes of an overlay vertex: position and color.
pub const VERTEX_SIZE: u64 = 24;
pub const ACCENT: [f32; 3] = [1.0, 0.63, 0.16];

const BALL_RADIUS: f32 = 46.0;
const BALL_MARGIN: f32 = 14.0;
const HANDLE_REACH: f32 = 82.0;
const GRAB: f32 = 8.0;

/// A flat drawing, in pixels of a screen of a size.
pub struct Overlay {
    pub bytes: Vec<u8>,
    size: (f32, f32),
}

type At = (f32, f32);

impl Overlay {
    #[must_use]
    pub fn new(size: At) -> Overlay {
        Overlay { bytes: Vec::new(), size }
    }

    fn vertex(&mut self, at: At, color: [f32; 4]) {
        let (x, y) = (at.0 / self.size.0 * 2.0 - 1.0, 1.0 - at.1 / self.size.1 * 2.0);
        for value in [x, y].into_iter().chain(color) {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        }
    }

    pub fn triangle(&mut self, corners: [At; 3], color: [f32; 4]) {
        for corner in corners {
            self.vertex(corner, color);
        }
    }

    pub fn line(&mut self, a: At, b: At, width: f32, color: [f32; 4]) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let size = dx.hypot(dy).max(1e-3);
        let (nx, ny) = (-dy / size * width * 0.5, dx / size * width * 0.5);
        let (first, second) = ((a.0 + nx, a.1 + ny), (a.0 - nx, a.1 - ny));
        let (third, fourth) = ((b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny));
        self.triangle([first, second, third], color);
        self.triangle([second, fourth, third], color);
    }

    pub fn disc(&mut self, center: At, radius: f32, color: [f32; 4]) {
        const STEPS: u16 = 24;
        let at = |i: u16| {
            let (s, c) = (f32::from(i) / f32::from(STEPS) * std::f32::consts::TAU).sin_cos();
            (center.0 + c * radius, center.1 + s * radius)
        };
        for i in 0..STEPS {
            self.triangle([center, at(i), at(i + 1)], color);
        }
    }

    fn polyline(&mut self, points: &[At], width: f32, color: [f32; 4]) {
        for pair in points.windows(2) {
            self.line(pair[0], pair[1], width, color);
        }
    }

    /// An X, a Y or a Z, on a disc.
    fn letter(&mut self, axis: usize, at: At, color: [f32; 4]) {
        let r = 4.0;
        let (x, y) = at;
        match axis {
            0 => {
                self.line((x - r, y - r), (x + r, y + r), 1.8, color);
                self.line((x - r, y + r), (x + r, y - r), 1.8, color);
            }
            1 => {
                self.line((x - r, y - r), (x, y), 1.8, color);
                self.line((x + r, y - r), (x, y), 1.8, color);
                self.line((x, y), (x, y + r), 1.8, color);
            }
            _ => {
                self.line((x - r, y - r), (x + r, y - r), 1.8, color);
                self.line((x + r, y - r), (x - r, y + r), 1.8, color);
                self.line((x - r, y + r), (x + r, y + r), 1.8, color);
            }
        }
    }
}

fn distance_to_segment(point: At, a: At, b: At) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let along = ((point.0 - a.0) * dx + (point.1 - a.1) * dy) / (dx * dx + dy * dy).max(1e-6);
    let t = along.clamp(0.0, 1.0);
    (point.0 - (a.0 + dx * t)).hypot(point.1 - (a.1 + dy * t))
}

fn opaque(color: [f32; 3], alpha: f32) -> [f32; 4] {
    [color[0], color[1], color[2], alpha]
}

/// The ball in the corner: where each axis ends, seen from the camera.
pub struct Ball {
    center: At,
    /// Each end: its axis, which way, where it is on screen and how near.
    ends: Vec<(usize, f32, At, f32)>,
}

impl Ball {
    #[must_use]
    pub fn new(camera: &Camera, size: At) -> Ball {
        let center = (size.0 - BALL_RADIUS - BALL_MARGIN, BALL_RADIUS + BALL_MARGIN);
        let (right, up, back) = camera.axes();
        let mut ends = Vec::with_capacity(6);
        for (axis, direction) in AXES.iter().enumerate() {
            for sign in [1.0, -1.0] {
                let end = scale(*direction, sign);
                ends.push((
                    axis,
                    sign,
                    (center.0 + dot(end, right) * BALL_RADIUS * 0.72, center.1 - dot(end, up) * BALL_RADIUS * 0.72),
                    dot(end, back),
                ));
            }
        }
        ends.sort_by(|a, b| a.3.total_cmp(&b.3));
        Ball { center, ends }
    }

    fn inside(&self, at: At) -> bool {
        (at.0 - self.center.0).hypot(at.1 - self.center.1) <= BALL_RADIUS
    }

    /// The end of an axis under a point, nearest the eye first.
    #[must_use]
    pub fn end_at(&self, at: At) -> Option<V3> {
        let under = self.ends.iter().rev().find(|(_, _, end, _)| (at.0 - end.0).hypot(at.1 - end.1) <= 11.0)?;
        Some(scale(AXES[under.0], under.1))
    }

    #[must_use]
    pub fn holds(&self, at: Option<At>) -> bool {
        at.is_some_and(|at| self.inside(at))
    }

    pub fn draw(&self, overlay: &mut Overlay, hovered: Option<At>) {
        if self.holds(hovered) {
            overlay.disc(self.center, BALL_RADIUS, [1.0, 1.0, 1.0, 0.07]);
        }
        for (axis, sign, end, _) in &self.ends {
            let color = AXIS_COLORS[*axis];
            let under = hovered.is_some_and(|at| (at.0 - end.0).hypot(at.1 - end.1) <= 11.0);
            if *sign > 0.0 {
                overlay.line(self.center, *end, 2.2, opaque(color, 0.9));
                overlay.disc(*end, 9.5, opaque(if under { [1.0, 1.0, 1.0] } else { color }, 1.0));
                overlay.letter(*axis, *end, [0.05, 0.05, 0.06, 1.0]);
            } else {
                overlay.disc(*end, 8.0, opaque(color, if under { 0.75 } else { 0.32 }));
            }
        }
    }
}

/// What a handle does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Move(usize),
    Turn(usize),
}

/// The handles on what is selected: an arrow and a ring an axis.
pub struct Handles {
    pub center: At,
    tips: [Option<At>; 3],
    rings: [Vec<At>; 3],
    /// World units of the arrows' length.
    reach: f32,
}

impl Handles {
    #[must_use]
    pub fn new(camera: &Camera, position: V3, size: At) -> Option<Handles> {
        let project = camera.projector(size);
        let center = project(position)?;
        let reach = HANDLE_REACH * camera.units_per_pixel(position, size.1);
        let tips = AXES.map(|axis| project(add(position, scale(axis, reach))));
        let ring = |axis: V3| {
            let helper = if axis[1].abs() > 0.9 { AXES[0] } else { AXES[1] };
            let u = normalized(cross(axis, helper));
            let v = cross(axis, u);
            (0..=64u16)
                .filter_map(|i| {
                    let (s, c) = (f32::from(i) / 64.0 * std::f32::consts::TAU).sin_cos();
                    project(add(position, scale(add(scale(u, c), scale(v, s)), reach * 0.8)))
                })
                .collect()
        };
        Some(Handles { center, tips, rings: AXES.map(ring), reach })
    }

    /// The handle under a point.
    #[must_use]
    pub fn at(&self, point: At) -> Option<Handle> {
        let mut best: Option<(f32, Handle)> = None;
        let mut offer = |distance: f32, handle| {
            if distance < GRAB && best.is_none_or(|(known, _)| distance < known) {
                best = Some((distance, handle));
            }
        };
        for (axis, tip) in self.tips.iter().enumerate() {
            if let Some(tip) = tip {
                offer(distance_to_segment(point, self.center, *tip), Handle::Move(axis));
            }
        }
        for (axis, ring) in self.rings.iter().enumerate() {
            let nearest =
                ring.windows(2).map(|pair| distance_to_segment(point, pair[0], pair[1])).fold(f32::MAX, f32::min);
            offer(nearest, Handle::Turn(axis));
        }
        best.map(|(_, handle)| handle)
    }

    /// World units moved along an axis by a pointer's move.
    #[must_use]
    pub fn along(&self, axis: usize, by: At) -> f32 {
        let Some(tip) = self.tips[axis] else { return 0.0 };
        let (dx, dy) = (tip.0 - self.center.0, tip.1 - self.center.1);
        let pixels = dx.hypot(dy).max(1.0);
        (by.0 * dx + by.1 * dy) / pixels / pixels * self.reach
    }

    /// The angle a pointer's move turns a ring by, about its axis as seen.
    #[must_use]
    pub fn turned(&self, axis: V3, back: V3, from: At, to: At) -> f32 {
        let angle = |at: At| (at.1 - self.center.1).atan2(at.0 - self.center.0);
        let mut delta = angle(to) - angle(from);
        if delta > std::f32::consts::PI {
            delta -= std::f32::consts::TAU;
        } else if delta < -std::f32::consts::PI {
            delta += std::f32::consts::TAU;
        }
        // Screen angles run clockwise; a ring seen from behind runs the other way.
        if dot(axis, back) >= 0.0 { -delta } else { delta }
    }

    pub fn draw(&self, overlay: &mut Overlay, active: Option<Handle>) {
        let shown =
            |handle: Handle, axis: usize| if active == Some(handle) { [1.0, 1.0, 1.0] } else { AXIS_COLORS[axis] };
        for (axis, ring) in self.rings.iter().enumerate() {
            overlay.polyline(ring, 1.8, opaque(shown(Handle::Turn(axis), axis), 0.85));
        }
        for (axis, tip) in self.tips.iter().enumerate() {
            let Some(tip) = *tip else { continue };
            let color = opaque(shown(Handle::Move(axis), axis), 1.0);
            overlay.line(self.center, tip, 2.4, color);
            let (dx, dy) = (tip.0 - self.center.0, tip.1 - self.center.1);
            let size = dx.hypot(dy).max(1e-3);
            let (ux, uy) = (dx / size, dy / size);
            let (base, end) = ((tip.0 - ux * 12.0, tip.1 - uy * 12.0), (tip.0 + ux * 2.0, tip.1 + uy * 2.0));
            overlay
                .triangle([end, (base.0 - uy * 5.0, base.1 + ux * 5.0), (base.0 + uy * 5.0, base.1 - ux * 5.0)], color);
        }
        overlay.disc(self.center, 3.5, [1.0, 1.0, 1.0, 1.0]);
    }
}

/// The bones, as lines from each part to its parent and a dot on each.
pub fn draw_bones(
    overlay: &mut Overlay,
    camera: &Camera,
    bones: &[(V3, Option<V3>)],
    selected: Option<usize>,
    size: At,
) {
    let project = camera.projector(size);
    for (index, (at, parent)) in bones.iter().enumerate() {
        let Some(here) = project(*at) else { continue };
        let color = if selected == Some(index) { opaque(ACCENT, 1.0) } else { [0.82, 0.86, 0.92, 0.85] };
        if let Some(there) = parent.and_then(&project) {
            overlay.line(there, here, 1.6, color);
        }
        overlay.disc(here, if selected == Some(index) { 4.0 } else { 2.6 }, color);
    }
}

/// The floor's step: a power of ten, so that it reads as a scale, of what is shown.
#[must_use]
pub fn floor_step(radius: f32) -> f32 {
    10f32.powf(radius.max(1.0).log10().floor()) / 2.0
}

/// The floor: as far as it fades to.
#[must_use]
pub fn floor_quad(step: f32) -> Vec<u8> {
    let reach = step * 24.0;
    let corner = |x: f32, z: f32| [x, 0.0, z, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    let corners = [
        corner(-reach, -reach),
        corner(reach, -reach),
        corner(reach, reach),
        corner(-reach, -reach),
        corner(reach, reach),
        corner(-reach, reach),
    ];
    corners.iter().flatten().flat_map(|value| value.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ball_shows_what_faces_the_camera_and_gives_it_back() {
        let camera = Camera { yaw: 0.0, pitch: 0.0, ..Camera::default() };
        let ball = Ball::new(&camera, (800.0, 600.0));
        // Looking along -Z: +X is right of the center, and +Y above it.
        let center = (800.0 - BALL_RADIUS - BALL_MARGIN, BALL_RADIUS + BALL_MARGIN);
        assert_eq!(ball.end_at((center.0 + BALL_RADIUS * 0.72, center.1)), Some(AXES[0]));
        assert_eq!(ball.end_at((center.0, center.1 - BALL_RADIUS * 0.72)), Some(AXES[1]));
    }

    #[test]
    fn a_handle_is_found_where_it_is_drawn_and_moves_as_far_as_the_pointer() {
        let camera = Camera { yaw: 0.0, pitch: 0.0, distance: 100.0, ..Camera::default() };
        let size = (800.0, 600.0);
        let handles = Handles::new(&camera, [0.0; 3], size).unwrap();
        let tip = handles.tips[0].unwrap();
        let middle = (f32::midpoint(handles.center.0, tip.0), f32::midpoint(handles.center.1, tip.1));
        assert_eq!(handles.at(middle), Some(Handle::Move(0)));
        assert_eq!(handles.at((5.0, 5.0)), None);
        let pixels = tip.0 - handles.center.0;
        let moved = handles.along(0, (pixels, 0.0));
        assert!((moved - handles.reach).abs() < 1e-3);
    }

    #[test]
    fn a_floor_is_scaled_to_what_stands_on_it() {
        assert_eq!(floor_step(900.0), 50.0);
        assert_eq!(floor_step(40_000.0), 5000.0);
        assert_eq!(floor_quad(10.0).len() as u64, 6 * crate::scene::VERTEX_SIZE);
    }
}
