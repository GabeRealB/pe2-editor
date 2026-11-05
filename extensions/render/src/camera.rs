//! A view around a point, as Blender's: it orbits, pans and zooms about that point, in perspective or orthographic. Y is up, and the camera looks along its own -Z.

use crate::math::{M4, V3, add, cross, dot, multiply, normalized, scale, sub, transform};

const NEAR_FACTOR: f32 = 0.01;
const FAR_FACTOR: f32 = 50.0;
/// How far the screen is from the eye, the screen being two high: a lens.
const FOCAL: f32 = 2.2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: V3,
    /// Radians about Y and above the horizon.
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub orthographic: bool,
}

impl Camera {
    /// As text, for `resumed`.
    #[must_use]
    pub fn written(&self) -> String {
        let [x, y, z] = self.target;
        format!("{x} {y} {z} {} {} {} {}", self.yaw, self.pitch, self.distance, u8::from(self.orthographic))
    }

    /// One `written` left.
    #[must_use]
    pub fn resumed(text: &str) -> Option<Camera> {
        let mut numbers = text.split(' ').map(str::parse::<f32>);
        let mut next = || numbers.next()?.ok();
        let target = [next()?, next()?, next()?];
        let (yaw, pitch, distance, orthographic) = (next()?, next()?, next()?, next()?);
        Some(Camera { target, yaw, pitch, distance, orthographic: orthographic > 0.5 })
    }
}

impl Default for Camera {
    fn default() -> Camera {
        Camera { target: [0.0; 3], yaw: 0.6, pitch: 0.35, distance: 10.0, orthographic: false }
    }
}

impl Camera {
    /// From the eye towards the target's opposite: the direction the camera is behind the target by.
    fn back(&self) -> V3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        [sy * cp, sp, cy * cp]
    }

    pub fn eye(&self) -> V3 {
        add(self.target, scale(self.back(), self.distance))
    }

    /// The camera's axes: right, up and back.
    pub fn axes(&self) -> (V3, V3, V3) {
        let back = self.back();
        let world_up =
            if self.pitch.cos().abs() < 1e-4 { [-self.yaw.sin(), 0.0, -self.yaw.cos()] } else { [0.0, 1.0, 0.0] };
        let right = normalized(cross(world_up, back));
        (right, cross(back, right), back)
    }

    fn view(&self) -> M4 {
        let (right, up, back) = self.axes();
        let eye = self.eye();
        [
            [right[0], up[0], back[0], 0.0],
            [right[1], up[1], back[1], 0.0],
            [right[2], up[2], back[2], 0.0],
            [-dot(right, eye), -dot(up, eye), -dot(back, eye), 1.0],
        ]
    }

    fn projection(&self, aspect: f32) -> M4 {
        let (near, far) = (self.distance * NEAR_FACTOR, self.distance * FAR_FACTOR);
        let depth = far / (near - far);
        if self.orthographic {
            let scale_y = FOCAL / self.distance;
            return [
                [scale_y / aspect, 0.0, 0.0, 0.0],
                [0.0, scale_y, 0.0, 0.0],
                [0.0, 0.0, 1.0 / (near - far), 0.0],
                [0.0, 0.0, near / (near - far) + 0.5, 1.0],
            ];
        }
        [
            [FOCAL / aspect, 0.0, 0.0, 0.0],
            [0.0, FOCAL, 0.0, 0.0],
            [0.0, 0.0, depth, -1.0],
            [0.0, 0.0, near * depth, 0.0],
        ]
    }

    pub fn view_projection(&self, aspect: f32) -> M4 {
        multiply(&self.projection(aspect), &self.view())
    }

    /// Where points land, in pixels from the corner of a screen of this size, none behind the eye: the matrices worked out once.
    pub fn projector(&self, size: (f32, f32)) -> impl Fn(V3) -> Option<(f32, f32)> {
        let matrix = self.view_projection(size.0 / size.1);
        move |point| {
            let clip = transform(&matrix, point);
            (clip[3] > 1e-6).then(|| {
                (f32::midpoint(clip[0] / clip[3], 1.0) * size.0, f32::midpoint(1.0, -clip[1] / clip[3]) * size.1)
            })
        }
    }

    /// The ray through a pixel: where it starts, and its direction.
    pub fn ray(&self, at: (f32, f32), size: (f32, f32)) -> (V3, V3) {
        let (right, up, back) = self.axes();
        let x = (at.0 / size.0 * 2.0 - 1.0) * size.0 / size.1;
        let y = 1.0 - at.1 / size.1 * 2.0;
        if self.orthographic {
            let world = self.distance / FOCAL;
            let origin = add(self.eye(), add(scale(right, x * world), scale(up, y * world)));
            return (origin, scale(back, -1.0));
        }
        let forward = sub(add(scale(right, x), scale(up, y)), scale(back, FOCAL));
        (self.eye(), normalized(forward))
    }

    /// World units a pixel covers at a point.
    pub fn units_per_pixel(&self, point: V3, height: f32) -> f32 {
        let depth =
            if self.orthographic { self.distance } else { dot(sub(self.eye(), point), self.axes().2).max(1e-3) };
        depth * 2.0 / (FOCAL * height)
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
    }

    pub fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        let (right, up, _) = self.axes();
        let per_pixel = self.units_per_pixel(self.target, height);
        self.target = add(self.target, add(scale(right, -dx * per_pixel), scale(up, dy * per_pixel)));
    }

    pub fn zoom(&mut self, steps: f32) {
        self.distance = (self.distance * (1.0 - steps * 0.0025).clamp(0.5, 1.5)).max(1e-3);
    }

    /// Show a sphere whole.
    pub fn frame(&mut self, center: V3, radius: f32) {
        self.target = center;
        self.distance = radius.max(1.0) * FOCAL * 1.15;
    }

    /// Look along an axis, as Blender's number pad does.
    pub fn look_along(&mut self, axis: V3) {
        self.pitch = axis[1].clamp(-1.0, 1.0).asin();
        if axis[0].abs() + axis[2].abs() > 1e-4 {
            self.yaw = axis[0].atan2(axis[2]);
        }
        self.orthographic = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn the_target_is_the_middle_of_the_screen_and_a_ray_through_it_finds_it() {
        for orthographic in [false, true] {
            let camera = Camera { target: [10.0, 20.0, -5.0], yaw: 0.7, pitch: -0.3, distance: 40.0, orthographic };
            let (x, y) = camera.projector((800.0, 600.0))(camera.target).unwrap();
            assert!(near(x, 400.0) && near(y, 300.0), "{x} {y}");
            let (origin, direction) = camera.ray((400.0, 300.0), (800.0, 600.0));
            let along = dot(sub(camera.target, origin), direction);
            let closest = add(origin, scale(direction, along));
            assert!(crate::math::length(sub(closest, camera.target)) < 0.01);
        }
    }

    #[test]
    fn what_is_up_is_up_and_what_is_right_is_right() {
        let camera = Camera { yaw: 0.0, pitch: 0.0, ..Camera::default() };
        let (_, y) = camera.projector((100.0, 100.0))([0.0, 1.0, 0.0]).unwrap();
        let (x, _) = camera.projector((100.0, 100.0))([1.0, 0.0, 0.0]).unwrap();
        assert!(y < 50.0 && x > 50.0, "{x} {y}");
    }

    #[test]
    fn a_ray_lands_where_a_point_projected() {
        for orthographic in [false, true] {
            let camera = Camera { target: [1.0, 2.0, 3.0], yaw: 2.0, pitch: 0.8, distance: 30.0, orthographic };
            let size = (640.0, 480.0);
            let point = [4.0, -3.0, 6.0];
            let at = camera.projector(size)(point).unwrap();
            let (origin, direction) = camera.ray(at, size);
            let along = dot(sub(point, origin), direction);
            assert!(crate::math::length(sub(add(origin, scale(direction, along)), point)) < 0.05);
        }
    }

    #[test]
    fn a_point_a_view_looks_along_is_where_the_camera_is() {
        let mut camera = Camera::default();
        camera.look_along([0.0, 1.0, 0.0]);
        assert!(camera.eye()[1] > 9.0);
        camera.look_along([1.0, 0.0, 0.0]);
        assert!(camera.eye()[0] > 9.0);
        camera.look_along([0.0, 0.0, 1.0]);
        assert!(camera.eye()[2] > 9.0);
    }

    #[test]
    fn a_camera_comes_back_as_it_was_written() {
        let camera =
            Camera { target: [1.5, -2.25, 300.0], yaw: 0.125, pitch: -0.5, distance: 42.0, orthographic: true };
        assert_eq!(Camera::resumed(&camera.written()), Some(camera));
        assert_eq!(Camera::resumed("not one"), None);
    }
}
