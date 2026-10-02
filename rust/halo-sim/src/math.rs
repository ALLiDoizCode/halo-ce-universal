//! The simulation's own maths, for the results to be the same everywhere.
//!
//! `core` has no `sqrt`, `sin` or `cos`, and the platform's (the C runtime's,
//! a browser's) differ in the last digits from one to the next, so the native
//! and WebAssembly builds would drift apart. These are built only from IEEE-754
//! `f32` addition, subtraction, multiplication, division and comparison and
//! integer work, which are exact and the same on every target.

pub type Vec3 = [f32; 3];

/// The engine's `_real_epsilon`.
pub const EPSILON: f32 = 0.0001;

/// The square root, to within an ulp of the exactly rounded one. NaN for a
/// negative number, as the engine's would be.
pub fn sqrt(x: f32) -> f32 {
    if x == 0.0 || x == f32::INFINITY {
        return x;
    }
    if x.is_nan() || x < 0.0 {
        return f32::NAN;
    }
    // halve the exponent for a first guess within a few percent, then Newton:
    // each step squares the relative error
    let mut y = f32::from_bits((x.to_bits() >> 1) + 0x1FBD_1DF5);
    for _ in 0..4 {
        y = 0.5 * (y + x / y);
    }
    y
}

// (Cephes' constants as published, to more digits than an `f32` holds)
const DP1: f32 = 0.785_156_25;
#[allow(clippy::excessive_precision)]
const DP2: f32 = 2.418_756_484_985_351_6e-4;
#[allow(clippy::excessive_precision)]
const DP3: f32 = 3.774_894_977_445_941e-8;
const FOUR_OVER_PI: f32 = 1.273_239_5;

/// Sine and cosine of an angle in radians (Cephes' single-precision
/// algorithm: reduce to the octant, then a polynomial). Accurate to a few
/// ulps for angles of any size a player can face (up to about 8,000 radians).
pub fn sin_cos(angle: f32) -> (f32, f32) {
    let mut x = angle;
    let mut sign_sin = false;
    if x < 0.0 {
        x = -x;
        sign_sin = true;
    }
    let mut j = (x * FOUR_OVER_PI) as u32;
    let mut y = j as f32;
    if j & 1 == 1 {
        j += 1;
        y += 1.0;
    }
    j &= 7;
    let mut sign_cos = false;
    if j > 3 {
        sign_sin = !sign_sin;
        sign_cos = !sign_cos;
        j -= 4;
    }
    if j > 1 {
        sign_cos = !sign_cos;
    }
    x = ((x - y * DP1) - y * DP2) - y * DP3;
    let z = x * x;
    let sin_poly = x + x * z * (-1.666_665_5e-1 + z * (8.332_161e-3 + z * -1.951_529_6e-4));
    let cos_poly = 1.0 - 0.5 * z + z * z * (4.166_664_6e-2 + z * (-1.388_731_6e-3 + z * 2.443_315_7e-5));
    // octants 1 and 2 swap the two
    let (mut s, mut c) = if j == 1 || j == 2 { (cos_poly, sin_poly) } else { (sin_poly, cos_poly) };
    if sign_sin {
        s = -s;
    }
    if sign_cos {
        c = -c;
    }
    (s, c)
}

/// An angle wrapped into `[-pi, pi]`.
pub fn wrap_angle(angle: f32) -> f32 {
    const PI: f32 = core::f32::consts::PI;
    const TWO_PI: f32 = 2.0 * PI;
    let mut a = angle;
    // (a loop, not a division: a player's facing is already within a turn or two)
    let mut guard = 0;
    while a > PI && guard < 64 {
        a -= TWO_PI;
        guard += 1;
    }
    while a < -PI && guard < 64 {
        a += TWO_PI;
        guard += 1;
    }
    a
}

pub fn dot(a: &Vec3, b: &Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: &Vec3, b: &Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub fn sub(a: &Vec3, b: &Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add(a: &Vec3, b: &Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn scale(a: &Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn magnitude_squared(a: &Vec3) -> f32 {
    a[0] * a[0] + a[1] * a[1] + a[2] * a[2]
}

pub fn magnitude(a: &Vec3) -> f32 {
    sqrt(magnitude_squared(a))
}

/// `point + vector * t`, as the engine's `point_from_line3d` computes it.
pub fn along(point: &Vec3, vector: &Vec3, t: f32) -> Vec3 {
    [vector[0] * t + point[0], vector[1] * t + point[1], vector[2] * t + point[2]]
}

/// Scale `v` to length 1 and return its length, or leave it as it is and
/// return 0 when it is shorter than the engine's epsilon (`normalize3d`).
// (the engine's own form of the comparison, which a NaN length fails)
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn normalize(v: &mut Vec3) -> f32 {
    let magnitude = magnitude(v);
    if !(EPSILON > (magnitude - 0.0).abs()) {
        *v = scale(v, 1.0 / magnitude);
        magnitude
    } else {
        0.0
    }
}

/// The same for two components (`normalize2d`).
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn normalize2(v: &mut [f32; 2]) -> f32 {
    let magnitude = sqrt(v[0] * v[0] + v[1] * v[1]);
    if !(EPSILON > (magnitude - 0.0).abs()) {
        let s = 1.0 / magnitude;
        *v = [v[0] * s, v[1] * s];
        magnitude
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_roots_are_within_an_ulp_of_the_exact_ones() {
        for i in 1..20_000 {
            let x = i as f32 * 0.0137;
            let want = (x as f64).sqrt() as f32;
            let got = sqrt(x);
            assert!((got - want).abs() <= want * 2.0 * f32::EPSILON, "sqrt({x}) = {got}, want {want}");
        }
        assert_eq!(sqrt(0.0), 0.0);
        assert!(sqrt(-1.0).is_nan());
        assert!(sqrt(f32::NAN).is_nan());
        assert_eq!(sqrt(f32::INFINITY), f32::INFINITY);
    }

    #[test]
    fn sines_and_cosines_are_close_to_the_exact_ones_for_any_facing() {
        let mut worst = 0.0f64;
        for i in -4000..4000 {
            let a = i as f32 * 0.0123;
            let (s, c) = sin_cos(a);
            worst = worst.max((s as f64 - (a as f64).sin()).abs()).max((c as f64 - (a as f64).cos()).abs());
        }
        assert!(worst < 5e-7, "worst error {worst}");
    }

    #[test]
    fn facings_wrap_into_one_turn() {
        assert!((wrap_angle(7.0) - (7.0 - 2.0 * core::f32::consts::PI)).abs() < 1e-5);
        assert!((wrap_angle(-4.0) - (-4.0 + 2.0 * core::f32::consts::PI)).abs() < 1e-5);
        assert_eq!(wrap_angle(1.0), 1.0);
    }

    #[test]
    fn normalizing_a_tiny_vector_leaves_it_as_it_was() {
        let mut v = [0.00001, 0.0, 0.0];
        assert_eq!(normalize(&mut v), 0.0);
        assert_eq!(v, [0.00001, 0.0, 0.0]);
        let mut v = [3.0, 0.0, 4.0];
        assert!((normalize(&mut v) - 5.0).abs() < 1e-6);
    }
}
