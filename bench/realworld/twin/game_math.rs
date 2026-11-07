//! Game math as a game's frame uses it, with glam: each frame places a model with a scale, a
//! turn and a move, puts it through a camera and a projection, then moves every point of a cloud
//! by that matrix and lights it by the turned direction it faces. glam's f64 types, as Fennec's
//! `float` is 64-bit.
#![allow(deprecated)]

use std::f64::consts::PI;

use glam::{DMat4, DQuat, DVec3};

/// `count` points spread over a 20-wide cube, from a fixed seed, so both sides get the same ones.
fn cloud(count: usize) -> Vec<DVec3> {
    let mut points = Vec::with_capacity(count);
    let mut seed: i64 = 12345;
    for _ in 0..count {
        let mut parts = [0.0; 3];
        for part in &mut parts {
            seed = (seed * 1_103_515_245 + 12345) % 2_147_483_648;
            *part = seed as f64 / 2_147_483_648.0 * 20.0 - 10.0;
        }
        points.push(DVec3::from_array(parts));
    }
    points
}

fn main() {
    let points = cloud(50_000);
    let view = DMat4::look_at_rh(DVec3::new(0.0, 5.0, 30.0), DVec3::ZERO, DVec3::Y);
    let projection = DMat4::perspective_rh(PI / 3.0, 16.0 / 9.0, 0.1, 500.0);
    let axis = DVec3::new(0.3, 1.0, 0.2).normalize_or_zero();
    let light = DVec3::new(-0.4, 1.0, 0.5).normalize_or_zero();
    let mut total = 0.0;
    let mut lit = 0.0;
    for frame in 0..200 {
        let time = f64::from(frame) * 0.016;
        let turn = DQuat::from_axis_angle(axis, time);
        let model = DMat4::from_scale_rotation_translation(
            DVec3::splat(1.5),
            turn,
            DVec3::new(time.sin(), 0.0, time.cos()),
        );
        let clip = projection * view * model;
        for point in &points {
            let moved = clip.transform_point3(*point);
            total = total + moved.x + moved.y + moved.z;
            lit += (turn * point.normalize_or_zero()).dot(light).max(0.0);
        }
    }
    println!("{total:?} {lit:?}");
}
