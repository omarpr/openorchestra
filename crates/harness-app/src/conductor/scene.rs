//! The conductor in 3D: a small CPU sphere-tracer that draws him as lit,
//! shaded frames once, off the main thread.
//!
//! Why a renderer and not divs: gpui divs are flat rectangles with rounded
//! corners — no rotation, no gradients that follow a surface — so the
//! original pet could only ever look like a paper cut-out. Signed distance
//! fields give real round shapes, and lighting them (key + fill + rim, soft
//! shadows, ambient occlusion, specular) is what makes him read as solid.
//!
//! Everything here is pure: positions in, BGRA bytes out. No gpui, so the
//! frames are testable and can be rendered on worker threads.

use std::ops::{Add, Mul, Neg, Sub};

/// Rendered frame edge in physical pixels. Shown at half that in logical
/// pixels, so it stays crisp on a 2× display.
pub const FRAME_PX: u32 = 176;
/// Frames in one bar of the 4/4 beat pattern.
pub const CONDUCT_FRAMES: usize = 24;
/// Frames in the bow played when an agent finishes.
pub const BOW_FRAMES: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pose {
    /// Beating time; the frame within the bar.
    Conduct(usize),
    /// Hands folded, baton lowered.
    Rest,
    /// Baton raised and held: "your cue" while an agent waits on the user.
    Cue,
    /// The bow at the end of a piece; the frame within it.
    Bow(usize),
    /// Beating time with the baton while the free hand is raised to cue
    /// someone: some agents still playing, one waiting on you.
    ConductCue(usize),
}

/// Every frame the conductor can show, in the order [`frame_index`] uses.
/// Each pose comes with eyes open and closed so blinking needs no re-render;
/// the bow is eyes-closed only (he is savouring it).
pub fn all_frames() -> Vec<(Pose, bool)> {
    let mut v = Vec::new();
    for f in 0..CONDUCT_FRAMES {
        v.push((Pose::Conduct(f), true));
        v.push((Pose::Conduct(f), false));
    }
    for pose in [Pose::Rest, Pose::Cue] {
        v.push((pose, true));
        v.push((pose, false));
    }
    for f in 0..BOW_FRAMES {
        v.push((Pose::Bow(f), false));
    }
    for f in 0..CONDUCT_FRAMES {
        v.push((Pose::ConductCue(f), true));
        v.push((Pose::ConductCue(f), false));
    }
    v
}

/// Index of `(pose, eyes_open)` in [`all_frames`].
pub fn frame_index(pose: Pose, eyes_open: bool) -> usize {
    let blink = usize::from(!eyes_open);
    match pose {
        Pose::Conduct(f) => (f % CONDUCT_FRAMES) * 2 + blink,
        Pose::Rest => CONDUCT_FRAMES * 2 + blink,
        Pose::Cue => CONDUCT_FRAMES * 2 + 2 + blink,
        Pose::Bow(f) => CONDUCT_FRAMES * 2 + 4 + f.min(BOW_FRAMES - 1),
        Pose::ConductCue(f) => {
            CONDUCT_FRAMES * 2 + 4 + BOW_FRAMES + (f % CONDUCT_FRAMES) * 2 + blink
        }
    }
}

// ---- tiny vector math -------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

impl Add for V3 {
    type Output = V3;
    fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for V3 {
    type Output = V3;
    fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f32> for V3 {
    type Output = V3;
    fn mul(self, s: f32) -> V3 {
        v3(self.x * s, self.y * s, self.z * s)
    }
}
impl Neg for V3 {
    type Output = V3;
    fn neg(self) -> V3 {
        v3(-self.x, -self.y, -self.z)
    }
}

impl V3 {
    fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn len(self) -> f32 {
        self.dot(self).sqrt()
    }
    fn norm(self) -> V3 {
        let l = self.len();
        if l > 1e-6 { self * (1.0 / l) } else { self }
    }
    fn mulv(self, o: V3) -> V3 {
        v3(self.x * o.x, self.y * o.y, self.z * o.z)
    }
    fn abs(self) -> V3 {
        v3(self.x.abs(), self.y.abs(), self.z.abs())
    }
    fn max0(self) -> V3 {
        v3(self.x.max(0.0), self.y.max(0.0), self.z.max(0.0))
    }
}

fn mix(a: V3, b: V3, t: f32) -> V3 {
    a + (b - a) * t
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Polynomial smooth minimum: blends two shapes into one organic surface.
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b + (a - b) * h - k * h * (1.0 - h)
}

fn rot_x(p: V3, a: f32) -> V3 {
    let (s, c) = a.sin_cos();
    v3(p.x, p.y * c - p.z * s, p.y * s + p.z * c)
}

fn rot_y(p: V3, a: f32) -> V3 {
    let (s, c) = a.sin_cos();
    v3(p.x * c + p.z * s, p.y, -p.x * s + p.z * c)
}

// ---- materials --------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mat {
    Coat,
    Shirt,
    Skin,
    Hair,
    Eye,
    Glove,
    Baton,
    Tie,
    Wood,
}

struct Surface {
    albedo: V3,
    spec: f32,
    shininess: f32,
}

fn srgb_to_linear(c: V3) -> V3 {
    v3(c.x.powf(2.2), c.y.powf(2.2), c.z.powf(2.2))
}

// ---- primitives -------------------------------------------------------------

#[derive(Clone, Copy)]
enum Shape {
    Sphere { c: V3, r: f32 },
    Capsule { a: V3, b: V3, r: f32 },
    Ellipsoid { c: V3, r: V3 },
    RoundCone { a: V3, b: V3, ra: f32, rb: f32 },
    RoundBox { c: V3, half: V3, r: f32 },
}

impl Shape {
    fn dist(&self, p: V3) -> f32 {
        match *self {
            Shape::Sphere { c, r } => (p - c).len() - r,
            Shape::Capsule { a, b, r } => {
                let pa = p - a;
                let ba = b - a;
                let h = (pa.dot(ba) / ba.dot(ba).max(1e-6)).clamp(0.0, 1.0);
                (pa - ba * h).len() - r
            }
            Shape::Ellipsoid { c, r } => {
                // Iñigo Quilez's bound: exact enough for shading, and never
                // overshoots far enough to tunnel through a thin eyelid.
                let q = p - c;
                let k0 = v3(q.x / r.x, q.y / r.y, q.z / r.z).len();
                let k1 = v3(q.x / (r.x * r.x), q.y / (r.y * r.y), q.z / (r.z * r.z)).len();
                if k1 < 1e-6 {
                    -r.x.min(r.y).min(r.z)
                } else {
                    k0 * (k0 - 1.0) / k1
                }
            }
            Shape::RoundCone { a, b, ra, rb } => {
                // Capsule with a radius that tapers along its axis.
                let pa = p - a;
                let ba = b - a;
                let h = (pa.dot(ba) / ba.dot(ba).max(1e-6)).clamp(0.0, 1.0);
                (pa - ba * h).len() - (ra + (rb - ra) * h)
            }
            Shape::RoundBox { c, half, r } => {
                let q = (p - c).abs() - half;
                q.max0().len() + q.x.max(q.y.max(q.z)).min(0.0) - r
            }
        }
    }

    /// A sphere enclosing the shape, for skipping it when far away.
    fn bounds(&self) -> (V3, f32) {
        match *self {
            Shape::Sphere { c, r } => (c, r),
            Shape::Capsule { a, b, r } => ((a + b) * 0.5, (b - a).len() * 0.5 + r),
            Shape::Ellipsoid { c, r } => (c, r.x.max(r.y).max(r.z)),
            Shape::RoundCone { a, b, ra, rb } => ((a + b) * 0.5, (b - a).len() * 0.5 + ra.max(rb)),
            Shape::RoundBox { c, half, r } => (c, half.len() + r),
        }
    }
}

#[derive(Clone, Copy)]
struct Prim {
    shape: Shape,
    mat: Mat,
    /// Smooth-union radius with the previous primitive of the same material;
    /// 0 is a hard union.
    blend: f32,
    center: V3,
    radius: f32,
}

struct Scene {
    prims: Vec<Prim>,
    /// World positions of the cheeks, for the blush.
    cheeks: [V3; 2],
}

impl Scene {
    fn push(&mut self, shape: Shape, mat: Mat, blend: f32) {
        let (center, radius) = shape.bounds();
        self.prims.push(Prim {
            shape,
            mat,
            blend,
            center,
            radius,
        });
    }

    /// Distance to the nearest surface, and that surface's material.
    fn map(&self, p: V3) -> (f32, Mat) {
        let mut best = f32::MAX;
        let mut best_mat = Mat::Coat;
        let mut last_mat: Option<Mat> = None;
        let mut group = f32::MAX;
        for prim in &self.prims {
            // A primitive further than the current best by more than its
            // blend radius cannot change the result: skip the real SDF.
            let bound = (p - prim.center).len() - prim.radius;
            let d = if bound > best.min(group) + prim.blend + 0.05 {
                bound
            } else {
                prim.shape.dist(p)
            };
            if prim.blend > 0.0 && last_mat == Some(prim.mat) {
                group = smin(group, d, prim.blend);
            } else {
                group = d;
            }
            last_mat = Some(prim.mat);
            if group < best {
                best = group;
                best_mat = prim.mat;
            }
        }
        (best, best_mat)
    }

    fn dist(&self, p: V3) -> f32 {
        self.map(p).0
    }

    fn normal(&self, p: V3) -> V3 {
        // Tetrahedral differences: four evaluations instead of six.
        let e = 0.0015;
        let k0 = v3(1.0, -1.0, -1.0);
        let k1 = v3(-1.0, -1.0, 1.0);
        let k2 = v3(-1.0, 1.0, -1.0);
        let k3 = v3(1.0, 1.0, 1.0);
        (k0 * self.dist(p + k0 * e)
            + k1 * self.dist(p + k1 * e)
            + k2 * self.dist(p + k2 * e)
            + k3 * self.dist(p + k3 * e))
        .norm()
    }

    fn soft_shadow(&self, p: V3, l: V3) -> f32 {
        let mut res: f32 = 1.0;
        let mut t = 0.02;
        for _ in 0..24 {
            let h = self.dist(p + l * t);
            res = res.min(10.0 * h / t);
            if res < 0.02 {
                return 0.0;
            }
            t += h.clamp(0.02, 0.2);
            if t > 2.5 {
                break;
            }
        }
        res.clamp(0.0, 1.0)
    }

    fn ambient_occlusion(&self, p: V3, n: V3) -> f32 {
        let mut occ = 0.0;
        let mut weight = 1.0;
        for i in 1..=5 {
            let h = 0.025 * i as f32;
            occ += (h - self.dist(p + n * h)) * weight;
            weight *= 0.7;
        }
        (1.0 - 3.5 * occ).clamp(0.0, 1.0)
    }

    fn surface(&self, mat: Mat, p: V3, accent: V3) -> Surface {
        let s = |c: V3, spec: f32, shininess: f32| Surface {
            albedo: srgb_to_linear(c),
            spec,
            shininess,
        };
        match mat {
            Mat::Coat => s(v3(0.12, 0.13, 0.20), 0.30, 28.0),
            Mat::Shirt => s(v3(0.93, 0.93, 0.95), 0.10, 16.0),
            Mat::Skin => {
                // Rosy cheeks: a soft pink falloff around two points.
                let base = v3(0.96, 0.75, 0.62);
                let pink = v3(0.96, 0.55, 0.55);
                let ch = |c: V3| (-(p - c).dot(p - c) / 0.004).exp();
                let w = (ch(self.cheeks[0]) + ch(self.cheeks[1])).min(1.0);
                s(mix(base, pink, w * 0.6), 0.18, 22.0)
            }
            Mat::Hair => s(v3(0.92, 0.92, 0.97), 0.25, 14.0),
            Mat::Eye => s(v3(0.04, 0.04, 0.06), 1.0, 90.0),
            Mat::Glove => s(v3(0.97, 0.97, 0.97), 0.12, 10.0),
            Mat::Baton => s(v3(0.98, 0.96, 0.88), 0.5, 50.0),
            Mat::Tie => s(accent, 0.6, 40.0),
            Mat::Wood => {
                // An accent stripe across the podium's front panel.
                let stripe = (p.y + 0.80).abs() < 0.035 && p.z > 0.2;
                if stripe {
                    s(accent, 0.5, 40.0)
                } else {
                    // Faint grain so it isn't plastic.
                    let grain = 0.5 + 0.5 * (p.y * 90.0 + (p.x * 7.0).sin() * 2.0).sin();
                    s(
                        mix(v3(0.40, 0.23, 0.12), v3(0.48, 0.29, 0.15), grain),
                        0.35,
                        30.0,
                    )
                }
            }
        }
    }
}

// ---- the rig ----------------------------------------------------------------

/// Where every movable part goes for one frame, in upper-body coordinates.
struct Rig {
    /// Forward bow, radians, pivoting at the hips.
    lean: f32,
    /// Sideways sway of the upper body.
    sway: f32,
    /// Head nod on the beat.
    bob: f32,
    baton_hand: V3,
    free_hand: V3,
    baton_dir: V3,
    /// 1 = open, ~0 = closed.
    eyes: f32,
    brow_raise: f32,
}

const SHOULDER_R: V3 = v3(-0.27, -0.10, 0.0); // the baton side (his right)
const SHOULDER_L: V3 = v3(0.27, -0.10, 0.0);
const UPPER_ARM: f32 = 0.28;
const FOREARM: f32 = 0.28;

/// Closed Catmull-Rom spline through `pts` at `t` in [0, 1).
fn loop_spline(pts: &[V3], t: f32) -> V3 {
    let n = pts.len();
    let f = t.rem_euclid(1.0) * n as f32;
    let i = f.floor() as usize;
    let u = f - i as f32;
    let p0 = pts[(i + n - 1) % n];
    let p1 = pts[i % n];
    let p2 = pts[(i + 1) % n];
    let p3 = pts[(i + 2) % n];
    let u2 = u * u;
    let u3 = u2 * u;
    (p1 * 2.0
        + (p2 - p0) * u
        + (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * u2
        + (p1 * 3.0 - p0 - p2 * 3.0 + p3) * u3)
        * 0.5
}

/// The classic 4/4 pattern for the baton hand: down, in, out, up.
const BEAT_PATTERN: [V3; 5] = [
    v3(-0.30, 0.30, 0.30),  // top of the bar
    v3(-0.30, -0.20, 0.36), // 1: the downbeat
    v3(-0.02, -0.06, 0.38), // 2: across the body
    v3(-0.64, -0.08, 0.26), // 3: out wide
    v3(-0.44, 0.16, 0.32),  // 4: the upbeat
];

fn rig_for(pose: Pose, eyes_open: bool) -> Rig {
    let eyes = if eyes_open { 1.0 } else { 0.08 };
    let tau = std::f32::consts::TAU;
    match pose {
        Pose::ConductCue(f) => {
            // The same beat, with the free hand raised high, palm out, and
            // the brows up: "you — your entrance".
            let tau = std::f32::consts::TAU;
            let t = f as f32 / CONDUCT_FRAMES as f32;
            let mut rig = rig_for(Pose::Conduct(f), eyes_open);
            rig.free_hand = v3(0.60, 0.30 + 0.025 * (tau * 2.0 * t).sin(), 0.32);
            rig.brow_raise = 0.04;
            rig
        }
        Pose::Conduct(f) => {
            let t = f as f32 / CONDUCT_FRAMES as f32;
            let hand = loop_spline(&BEAT_PATTERN, t);
            let ahead = loop_spline(&BEAT_PATTERN, t + 0.03);
            // The baton trails its hand's motion a little: a whip, not a stick.
            let vel = (ahead - hand) * 6.0;
            let reach = (hand - SHOULDER_R).norm();
            Rig {
                lean: 0.05 * (tau * 2.0 * t).sin(),
                sway: 0.035 * (tau * t).sin(),
                bob: -0.018 * (tau * 4.0 * t).cos(),
                baton_hand: hand,
                free_hand: v3(0.42, -0.10 + 0.12 * (tau * t + 0.6).sin(), 0.30),
                baton_dir: (reach + v3(0.0, 0.9, 0.25) - vel).norm(),
                eyes,
                brow_raise: 0.012 * (tau * 4.0 * t).sin().max(0.0),
            }
        }
        Pose::Rest => Rig {
            lean: 0.0,
            sway: 0.0,
            bob: 0.0,
            baton_hand: v3(-0.12, -0.36, 0.30),
            free_hand: v3(0.10, -0.38, 0.31),
            baton_dir: v3(-0.7, -0.45, 0.4).norm(),
            eyes,
            brow_raise: 0.0,
        },
        Pose::Cue => Rig {
            lean: -0.06,
            sway: -0.02,
            bob: 0.01,
            baton_hand: v3(-0.40, 0.44, 0.26),
            free_hand: v3(0.44, 0.24, 0.34),
            baton_dir: v3(-0.15, 1.0, 0.35).norm(),
            eyes,
            brow_raise: 0.045,
        },
        Pose::Bow(f) => {
            let t = f as f32 / (BOW_FRAMES - 1) as f32;
            Rig {
                lean: 0.6 * (std::f32::consts::PI * t).sin(),
                sway: 0.0,
                bob: 0.0,
                baton_hand: v3(-0.62, -0.12, 0.16),
                free_hand: v3(0.06, -0.20, 0.30),
                baton_dir: v3(-0.8, -0.35, 0.25).norm(),
                eyes,
                brow_raise: 0.01,
            }
        }
    }
}

/// Two-bone inverse kinematics: where the elbow goes so both arm segments
/// keep their length, bending toward `pole`.
fn elbow(shoulder: V3, hand: V3, pole: V3) -> (V3, V3) {
    let max = UPPER_ARM + FOREARM - 0.005;
    let mut to = hand - shoulder;
    let mut d = to.len();
    if d > max {
        to = to * (max / d);
        d = max;
    }
    let hand = shoulder + to;
    let dir = to.norm();
    let a = (UPPER_ARM * UPPER_ARM - FOREARM * FOREARM + d * d) / (2.0 * d);
    let h = (UPPER_ARM * UPPER_ARM - a * a).max(0.0).sqrt();
    let side = (pole - dir * pole.dot(dir)).norm();
    (shoulder + dir * a + side * h, hand)
}

fn build_scene(pose: Pose, eyes_open: bool) -> Scene {
    let rig = rig_for(pose, eyes_open);
    let pivot = v3(0.0, -0.58, 0.0);
    // Upper-body local → world: sway, then bow forward about the hips.
    let up = |p: V3| -> V3 {
        let q = p + v3(rig.sway * ((p.y - pivot.y) / 0.9).clamp(0.0, 1.0), 0.0, 0.0) - pivot;
        rot_x(q, -rig.lean) + pivot
    };
    let head = |p: V3| up(p + v3(0.0, rig.bob, 0.0));

    let mut s = Scene {
        prims: Vec::with_capacity(40),
        cheeks: [head(v3(-0.15, 0.22, 0.20)), head(v3(0.15, 0.22, 0.20))],
    };

    // Podium first: it is the biggest thing and usually the nearest hit.
    s.push(
        Shape::RoundBox {
            c: v3(0.0, -0.90, 0.10),
            half: v3(0.46, 0.24, 0.26),
            r: 0.04,
        },
        Mat::Wood,
        0.0,
    );
    s.push(
        Shape::RoundBox {
            c: v3(0.0, -0.64, 0.10),
            half: v3(0.52, 0.025, 0.31),
            r: 0.02,
        },
        Mat::Wood,
        0.0,
    );

    // Torso: a tapered tailcoat.
    s.push(
        Shape::RoundCone {
            a: up(v3(0.0, -0.62, 0.0)),
            b: up(v3(0.0, -0.08, 0.0)),
            ra: 0.34,
            rb: 0.25,
        },
        Mat::Coat,
        0.0,
    );
    // Arms, blended into the coat at the shoulders.
    let (er, hr) = elbow(SHOULDER_R, rig.baton_hand, v3(-1.0, -0.7, -0.5));
    let (el, hl) = elbow(SHOULDER_L, rig.free_hand, v3(1.0, -0.7, -0.5));
    for (sh, el, ha) in [(SHOULDER_R, er, hr), (SHOULDER_L, el, hl)] {
        s.push(
            Shape::Capsule {
                a: up(sh),
                b: up(el),
                r: 0.072,
            },
            Mat::Coat,
            0.06,
        );
        s.push(
            Shape::Capsule {
                a: up(el),
                b: up(ha),
                r: 0.062,
            },
            Mat::Coat,
            0.03,
        );
    }
    // Shirt front and the neck.
    s.push(
        Shape::Ellipsoid {
            c: up(v3(0.0, -0.22, 0.19)),
            r: v3(0.11, 0.20, 0.10),
        },
        Mat::Shirt,
        0.0,
    );
    s.push(
        Shape::Capsule {
            a: up(v3(0.0, -0.06, 0.0)),
            b: head(v3(0.0, 0.10, 0.0)),
            r: 0.09,
        },
        Mat::Skin,
        0.0,
    );

    // Head and face.
    s.push(
        Shape::Sphere {
            c: head(v3(0.0, 0.27, 0.02)),
            r: 0.26,
        },
        Mat::Skin,
        0.0,
    );
    s.push(
        Shape::Sphere {
            c: head(v3(-0.26, 0.26, 0.0)),
            r: 0.055,
        },
        Mat::Skin,
        0.03,
    );
    s.push(
        Shape::Sphere {
            c: head(v3(0.26, 0.26, 0.0)),
            r: 0.055,
        },
        Mat::Skin,
        0.03,
    );
    s.push(
        Shape::Sphere {
            c: head(v3(0.0, 0.24, 0.27)),
            r: 0.055,
        },
        Mat::Skin,
        0.03,
    );
    // Wild white maestro hair, bald on top.
    for (c, r) in [
        (v3(-0.25, 0.33, -0.06), 0.12),
        (v3(0.25, 0.33, -0.06), 0.12),
        (v3(-0.20, 0.45, -0.10), 0.11),
        (v3(0.20, 0.45, -0.10), 0.11),
        (v3(0.0, 0.46, -0.15), 0.14),
        (v3(-0.30, 0.22, -0.08), 0.08),
        (v3(0.30, 0.22, -0.08), 0.08),
    ] {
        s.push(Shape::Sphere { c: head(c), r }, Mat::Hair, 0.07);
    }
    // Mustache.
    s.push(
        Shape::Ellipsoid {
            c: head(v3(-0.06, 0.175, 0.245)),
            r: v3(0.075, 0.028, 0.035),
        },
        Mat::Hair,
        0.0,
    );
    s.push(
        Shape::Ellipsoid {
            c: head(v3(0.06, 0.175, 0.245)),
            r: v3(0.075, 0.028, 0.035),
        },
        Mat::Hair,
        0.03,
    );
    // Brows.
    let br = rig.brow_raise;
    s.push(
        Shape::Capsule {
            a: head(v3(-0.05, 0.40 + br, 0.24)),
            b: head(v3(-0.15, 0.385 + br, 0.205)),
            r: 0.018,
        },
        Mat::Hair,
        0.0,
    );
    s.push(
        Shape::Capsule {
            a: head(v3(0.05, 0.40 + br, 0.24)),
            b: head(v3(0.15, 0.385 + br, 0.205)),
            r: 0.018,
        },
        Mat::Hair,
        0.0,
    );
    // Eyes: glossy beads that squash flat to blink.
    let eye_h = (0.048 * rig.eyes).max(0.007);
    for x in [-0.095, 0.095] {
        s.push(
            Shape::Ellipsoid {
                c: head(v3(x, 0.32, 0.225)),
                r: v3(0.04, eye_h, 0.03),
            },
            Mat::Eye,
            0.0,
        );
    }

    // Bow tie in the theme's accent.
    s.push(
        Shape::Ellipsoid {
            c: up(v3(-0.065, -0.02, 0.255)),
            r: v3(0.06, 0.04, 0.03),
        },
        Mat::Tie,
        0.0,
    );
    s.push(
        Shape::Ellipsoid {
            c: up(v3(0.065, -0.02, 0.255)),
            r: v3(0.06, 0.04, 0.03),
        },
        Mat::Tie,
        0.02,
    );
    s.push(
        Shape::Sphere {
            c: up(v3(0.0, -0.02, 0.27)),
            r: 0.028,
        },
        Mat::Tie,
        0.02,
    );

    // White gloves and the baton.
    // Gloves sit just past the cuff so the sleeve never pokes through.
    let past = |el: V3, ha: V3| ha + (ha - el).norm() * 0.035;
    s.push(
        Shape::Sphere {
            c: up(past(er, hr)),
            r: 0.07,
        },
        Mat::Glove,
        0.0,
    );
    s.push(
        Shape::Sphere {
            c: up(past(el, hl)),
            r: 0.075,
        },
        Mat::Glove,
        0.0,
    );
    let grip = past(er, hr);
    let tip = up(grip + rig.baton_dir * 0.50);
    s.push(
        Shape::Capsule {
            a: up(grip + rig.baton_dir * 0.03),
            b: tip,
            r: 0.013,
        },
        Mat::Baton,
        0.0,
    );

    s
}

// ---- the camera and the shading --------------------------------------------

/// World units across the frame, centred on the figure.
const VIEW_W: f32 = 2.2;
const VIEW_CY: f32 = -0.12;
/// Three-quarter view: turned a little toward his baton side, seen from a
/// touch above. Square-on, the lighting is all that says "3D".
const YAW: f32 = 0.38;
const PITCH: f32 = -0.16;

fn to_world(p: V3) -> V3 {
    rot_y(rot_x(p, PITCH), YAW)
}

/// Render one frame as BGRA with straight (non-premultiplied) alpha, which is
/// what gpui's sprite pipeline blends.
pub fn render_frame(pose: Pose, eyes_open: bool, accent: [f32; 3]) -> Vec<u8> {
    let scene = build_scene(pose, eyes_open);
    let accent = v3(accent[0], accent[1], accent[2]);
    let n = FRAME_PX as usize;
    let px = VIEW_W / n as f32;
    let dir = to_world(v3(0.0, 0.0, -1.0)).norm();
    let key = v3(-0.55, 0.75, 0.65).norm();
    let fill = v3(0.8, 0.15, 0.45).norm();
    let rim = v3(0.35, 0.45, -0.85).norm();
    let mut out = vec![0u8; n * n * 4];

    for j in 0..n {
        for i in 0..n {
            let u = (i as f32 + 0.5 - n as f32 / 2.0) * px;
            let v = (n as f32 / 2.0 - j as f32 - 0.5) * px + VIEW_CY;
            let origin = to_world(v3(u, v, 3.0));

            // Sphere-trace, remembering the closest pass for edge coverage.
            let mut t = 1.2;
            let mut min_d = f32::MAX;
            let mut t_min = t;
            let mut hit = false;
            for _ in 0..96 {
                let d = scene.dist(origin + dir * t);
                if d < min_d {
                    min_d = d;
                    t_min = t;
                }
                if d < 0.0008 {
                    hit = true;
                    break;
                }
                t += d * 0.9;
                if t > 5.0 {
                    break;
                }
            }

            // Ground shadow under the podium, drawn in screen space.
            let sh = {
                let ex = u / 0.78;
                let ey = (v + 1.16) / 0.07;
                let r = (ex * ex + ey * ey).sqrt();
                0.35 * (1.0 - smoothstep(0.4, 1.0, r))
            };

            let coverage = if hit {
                1.0
            } else {
                (1.0 - min_d / px).clamp(0.0, 1.0)
            };
            let o = (j * n + i) * 4;
            if coverage <= 0.0 {
                out[o + 3] = (sh * 255.0) as u8;
                continue;
            }

            let p = origin + dir * if hit { t } else { t_min };
            let (_, mat) = scene.map(p);
            let surf = scene.surface(mat, p, accent);
            let nrm = scene.normal(p);
            let view = -dir;
            let shadow = scene.soft_shadow(p + nrm * 0.004, key);
            let ao = scene.ambient_occlusion(p, nrm);

            let kd = nrm.dot(key).max(0.0);
            let fd = nrm.dot(fill).max(0.0);
            let sky = 0.5 + 0.5 * nrm.y;
            let ambient = mix(v3(0.10, 0.08, 0.07), v3(0.22, 0.25, 0.32), sky) * ao;
            let light = v3(1.0, 0.95, 0.88) * (kd * shadow * 1.15)
                + v3(0.30, 0.38, 0.52) * (fd * 0.55)
                + ambient;
            let mut col = surf.albedo.mulv(light);

            let half = (key + view).norm();
            let spec = nrm.dot(half).max(0.0).powf(surf.shininess) * surf.spec * shadow;
            col = col + v3(1.0, 0.97, 0.92) * spec;

            let fres = (1.0 - nrm.dot(view).max(0.0)).powi(3);
            let rim_l = nrm.dot(rim).max(0.0) * 0.6 + 0.4;
            col = col + v3(0.55, 0.65, 0.85) * (fres * rim_l * 0.45 * ao);

            // Filmic-ish rolloff so the highlights don't clip, then sRGB.
            let tone = |c: f32| (c / (1.0 + c * 0.25)).max(0.0).powf(1.0 / 2.2).min(1.0);
            let (r, g, b) = (tone(col.x), tone(col.y), tone(col.z));
            // Composite over the ground shadow (black at alpha `sh`).
            let a = coverage + sh * (1.0 - coverage);
            let k = if a > 0.0 { coverage / a } else { 0.0 };
            out[o] = (b * k * 255.0).round() as u8;
            out[o + 1] = (g * k * 255.0).round() as u8;
            out[o + 2] = (r * k * 255.0).round() as u8;
            out[o + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}

/// Render every frame of [`all_frames`] across the machine's cores.
pub fn render_all(accent: [f32; 3]) -> Vec<Vec<u8>> {
    let specs = all_frames();
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 8);
    let mut out: Vec<Vec<u8>> = vec![Vec::new(); specs.len()];
    std::thread::scope(|scope| {
        let chunks = out.chunks_mut(specs.len().div_ceil(threads));
        for (ci, chunk) in chunks.enumerate() {
            let base = ci * specs.len().div_ceil(threads);
            let specs = &specs;
            scope.spawn(move || {
                for (k, slot) in chunk.iter_mut().enumerate() {
                    let (pose, eyes) = specs[base + k];
                    *slot = render_frame(pose, eyes, accent);
                }
            });
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_index_matches_all_frames_order() {
        for (i, (pose, eyes)) in all_frames().into_iter().enumerate() {
            assert_eq!(frame_index(pose, eyes), i, "{pose:?} eyes={eyes}");
        }
    }

    #[test]
    fn a_frame_has_a_solid_figure_and_a_transparent_background() {
        let px = render_frame(Pose::Rest, true, [0.36, 0.61, 1.0]);
        assert_eq!(px.len(), (FRAME_PX * FRAME_PX * 4) as usize);
        let alpha = |x: u32, y: u32| px[((y * FRAME_PX + x) * 4 + 3) as usize];
        // Top corners are empty sky; the middle of the chest is opaque.
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(FRAME_PX - 1, 0), 0);
        assert_eq!(alpha(FRAME_PX / 2, FRAME_PX / 2), 255);
    }

    #[test]
    fn blinking_changes_the_face_and_nothing_else_much() {
        let open = render_frame(Pose::Rest, true, [0.36, 0.61, 1.0]);
        let shut = render_frame(Pose::Rest, false, [0.36, 0.61, 1.0]);
        let differing = open
            .chunks(4)
            .zip(shut.chunks(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(differing > 10, "a blink must be visible ({differing} px)");
        assert!(
            differing < 1500,
            "a blink must not move the body ({differing} px)"
        );
    }

    #[test]
    fn the_baton_moves_through_the_bar() {
        let a = render_frame(Pose::Conduct(0), true, [0.36, 0.61, 1.0]);
        let b = render_frame(Pose::Conduct(CONDUCT_FRAMES / 4), true, [0.36, 0.61, 1.0]);
        let differing = a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x != y).count();
        assert!(
            differing > 500,
            "beats must look different ({differing} px)"
        );
    }

    /// Writes every frame as a contact sheet BMP for eyeballing the art:
    /// `CONDUCTOR_SHEET=/tmp/sheet.bmp cargo test --release conductor_sheet -- --ignored`
    #[test]
    #[ignore]
    fn conductor_sheet() {
        let path = std::env::var("CONDUCTOR_SHEET").unwrap_or_else(|_| "conductor.bmp".into());
        let started = std::time::Instant::now();
        let frames = render_all([0.36, 0.61, 1.0]);
        eprintln!(
            "rendered {} frames in {:?}",
            frames.len(),
            started.elapsed()
        );
        let cols = 8usize;
        let rows = frames.len().div_ceil(cols);
        let f = FRAME_PX as usize;
        let (w, h) = (cols * f, rows * f);
        let mut img = vec![0u8; w * h * 4];
        for (k, frame) in frames.iter().enumerate() {
            let (cx, cy) = ((k % cols) * f, (k / cols) * f);
            for y in 0..f {
                for x in 0..f {
                    let s = (y * f + x) * 4;
                    let a = frame[s + 3] as f32 / 255.0;
                    // Composite onto a mid-dark card colour.
                    let bg = [0x2a as f32, 0x26 as f32, 0x22 as f32];
                    let d = ((cy + y) * w + cx + x) * 4;
                    for c in 0..3 {
                        img[d + c] = (frame[s + c] as f32 * a + bg[c] * (1.0 - a)) as u8;
                    }
                    img[d + 3] = 255;
                }
            }
        }
        // 32-bit top-down BMP.
        let mut bmp = Vec::new();
        let size = 54 + img.len();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(size as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&(w as i32).to_le_bytes());
        bmp.extend_from_slice(&(-(h as i32)).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 24]);
        bmp.extend_from_slice(&img);
        std::fs::write(&path, bmp).unwrap();
    }
}
