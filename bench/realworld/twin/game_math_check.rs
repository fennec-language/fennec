//! What glam gives for every line `examples/game_math.fen` prints, on the same inputs, for
//! `crates/fennec/tests/game_math.rs` to compare with. Each float is printed so it reads back as
//! the same `f64`. Write the fixture again with
//! `cargo run --release --bin game_math_check > ../../../crates/fennec/tests/fixtures/game_math.glam`.
#![allow(deprecated)]

use std::f64::consts::PI;

use glam::{DMat3, DMat4, DQuat, DVec2, DVec3, DVec4, EulerRot};

fn line(label: &str, values: &[f64]) {
    let parts: Vec<String> = values.iter().map(|value| format!("{value:?}")).collect();
    println!("{label}: {}", parts.join(" "));
}

fn v2(label: &str, v: DVec2) {
    line(label, &v.to_array());
}

fn v3(label: &str, v: DVec3) {
    line(label, &v.to_array());
}

fn v4(label: &str, v: DVec4) {
    line(label, &v.to_array());
}

fn q(label: &str, q: DQuat) {
    line(label, &q.to_array());
}

fn m3(label: &str, m: DMat3) {
    line(label, &m.to_cols_array());
}

fn m4(label: &str, m: DMat4) {
    line(label, &m.to_cols_array());
}

fn main() {
    let flat = DVec2::new(3.0, -4.0);
    let aside = DVec2::new(0.25, 7.5);
    v2("vec2 add", flat + aside);
    v2("vec2 sub", flat - aside);
    v2("vec2 scale", flat * 1.75);
    v2("vec2 mul", flat * aside);
    line("vec2 dot", &[flat.dot(aside)]);
    line("vec2 length", &[flat.length()]);
    v2("vec2 normalize", flat.normalize_or_zero());
    line("vec2 distance", &[flat.distance(aside)]);
    v2("vec2 lerp", flat.lerp(aside, 0.3));
    v2("vec2 min", flat.min(aside));
    v2("vec2 max", flat.max(aside));

    let a = DVec3::new(1.5, -2.25, 0.75);
    let b = DVec3::new(-0.5, 4.0, 3.125);
    v3("vec3 add", a + b);
    v3("vec3 sub", a - b);
    v3("vec3 scale", a * -0.6);
    v3("vec3 mul", a * b);
    v3("vec3 negated", -a);
    line("vec3 dot", &[a.dot(b)]);
    v3("vec3 cross", a.cross(b));
    line("vec3 length", &[a.length()]);
    line("vec3 length_squared", &[a.length_squared()]);
    v3("vec3 normalize", a.normalize_or_zero());
    v3("vec3 normalize zero", DVec3::ZERO.normalize_or_zero());
    line("vec3 distance", &[a.distance(b)]);
    v3("vec3 lerp", a.lerp(b, 0.7));
    v3("vec3 min", a.min(b));
    v3("vec3 max", a.max(b));
    v3("vec3 abs", a.abs());

    let c = DVec4::new(0.1, 0.2, -0.3, 1.0);
    let d = DVec4::new(2.0, -1.0, 0.5, 0.25);
    v4("vec4 add", c + d);
    v4("vec4 sub", c - d);
    v4("vec4 scale", c * 3.0);
    line("vec4 dot", &[c.dot(d)]);
    line("vec4 length", &[c.length()]);
    v4("vec4 normalize", c.normalize_or_zero());
    v4("vec4 lerp", c.lerp(d, 0.25));

    let axis = DVec3::new(1.0, 2.0, 2.0).normalize_or_zero();
    let turn = DQuat::from_axis_angle(axis, 0.8);
    let other = DQuat::from_axis_angle(DVec3::Z, -1.3);
    q("quat from_axis_angle", turn);
    q("quat from_rotation_x", DQuat::from_rotation_x(0.4));
    q("quat from_rotation_y", DQuat::from_rotation_y(-1.1));
    q("quat from_rotation_z", DQuat::from_rotation_z(2.5));
    q("quat from_euler", DQuat::from_euler(EulerRot::YXZ, 0.3, -0.2, 0.9));
    q("quat mul", turn * other);
    v3("quat rotate", turn * a);
    line("quat dot", &[turn.dot(other)]);
    line("quat length", &[turn.length()]);
    q("quat normalize", DQuat::from_xyzw(1.0, 2.0, 3.0, 4.0).normalize());
    q("quat conjugate", turn.conjugate());
    q("quat inverse", turn.inverse());
    q("quat lerp", turn.lerp(other, 0.4));
    q("quat slerp", turn.slerp(other, 0.4));
    q("quat slerp near", turn.slerp(turn, 0.5));
    q("quat slerp far", turn.slerp(other.conjugate(), 0.6));

    let turned = DMat3::from_quat(turn);
    let skew = DMat3::from_cols(
        DVec3::new(2.0, 0.5, -1.0),
        DVec3::new(0.0, 3.0, 0.25),
        DVec3::new(1.0, -2.0, 1.5),
    );
    m3("mat3 from_quat", turned);
    m3("mat3 mul", turned * skew);
    v3("mat3 mul_vec3", skew * b);
    m3("mat3 transpose", skew.transpose());
    line("mat3 determinant", &[skew.determinant()]);
    m3("mat3 inverse", skew.inverse());

    let place = DMat4::from_scale_rotation_translation(
        DVec3::new(2.0, 0.5, 1.25),
        turn,
        DVec3::new(10.0, -3.0, 4.5),
    );
    let view = DMat4::look_at_rh(DVec3::new(4.0, 3.0, 8.0), DVec3::new(0.0, 1.0, 0.0), DVec3::Y);
    let projection = DMat4::perspective_rh(PI / 3.0, 16.0 / 9.0, 0.1, 500.0);
    m4("mat4 translation", DMat4::from_translation(b));
    m4("mat4 rotation", DMat4::from_quat(other));
    m4("mat4 scaling", DMat4::from_scale(a));
    m4("mat4 from_scale_rotation_translation", place);
    m4("mat4 look_at", view);
    m4("mat4 perspective", projection);
    m4("mat4 mul", projection * view * place);
    v4("mat4 mul_vec4", place * d);
    v3("mat4 transform_point", place.transform_point3(a));
    v3("mat4 transform_vector", place.transform_vector3(a));
    m4("mat4 transpose", place.transpose());
    line("mat4 determinant", &[place.determinant()]);
    m4("mat4 inverse", place.inverse());
    m4("mat4 inverse view", view.inverse());
    let flat_matrix = DMat4::from_scale(DVec3::new(1.0, 0.0, 1.0));
    let none = if flat_matrix.try_inverse().is_none() { "True" } else { "False" };
    println!("mat4 inverse flat: {none}");
}
