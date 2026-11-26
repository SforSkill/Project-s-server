//! Predictive interest management (PIM).
//!
//! The server only sends an enemy to a client if that enemy could become
//! visible to the client within a short look-ahead window. A wallhack therefore
//! has nothing to show behind walls or smokes, while a peeking enemy is
//! already present on the client before it appears around the corner
//! (no "pop-in"): both the viewer's eye and the target are extrapolated along
//! their velocity, plus a margin covering the peeker's advantage.

use engine_physics::CollisionWorld;
use glam::Vec3;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisibilityConfig {
    pub enabled: bool,
    /// Extra look-ahead on top of the viewer's RTT/2 + interpolation, seconds.
    pub lookahead: f32,
    /// Sideways margin added around both eye and target (units).
    pub margin: f32,
    /// Enemies closer than this are always sent (melee, audible).
    pub always_within: f32,
    /// Keep sending for this long after the last positive test (anti-flicker).
    pub hold_time: f32,
}

impl Default for VisibilityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lookahead: 0.1,
            margin: 24.0,
            always_within: 160.0,
            hold_time: 0.25,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Viewer {
    pub eye: Vec3,
    pub velocity: Vec3,
    /// Network delay to compensate: RTT/2 + interpolation delay, seconds.
    pub delay: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub origin: Vec3,
    pub velocity: Vec3,
    /// Hull height (72 standing, 54 crouched).
    pub height: f32,
}

/// Sample points on a target hull: head, chest, feet corners, sides.
fn target_points(origin: Vec3, height: f32, side: Vec3, margin: f32) -> [Vec3; 7] {
    let up = Vec3::Y;
    [
        origin + up * (height - 6.0),
        origin + up * (height * 0.6),
        origin + up * 8.0,
        origin + up * (height * 0.6) + side * (16.0 + margin),
        origin + up * (height * 0.6) - side * (16.0 + margin),
        origin + up * (height - 6.0) + side * (16.0 + margin),
        origin + up * (height - 6.0) - side * (16.0 + margin),
    ]
}

/// Whether `target` may be visible from `viewer` within the look-ahead window.
/// `blocked(a, b)` adds dynamic occluders (smokes); `world` gives the walls.
pub fn potentially_visible(
    world: &CollisionWorld,
    cfg: &VisibilityConfig,
    viewer: &Viewer,
    target: &Target,
    blocked: &dyn Fn(Vec3, Vec3) -> bool,
) -> bool {
    if !cfg.enabled {
        return true;
    }
    let to = target.origin + Vec3::Y * (target.height * 0.5) - viewer.eye;
    if to.length() < cfg.always_within {
        return true;
    }
    let horizontal = Vec3::new(to.x, 0.0, to.z).normalize_or_zero();
    let side = Vec3::new(-horizontal.z, 0.0, horizontal.x);
    let t = viewer.delay + cfg.lookahead;
    let cap = |v: Vec3| {
        let h = Vec3::new(v.x, 0.0, v.z);
        let l = h.length();
        if l > 300.0 { h * (300.0 / l) } else { h }
    };
    // Viewer eyes: now, predicted, and peeking sideways.
    let eyes = [
        viewer.eye,
        viewer.eye + cap(viewer.velocity) * t,
        viewer.eye + side * cfg.margin,
        viewer.eye - side * cfg.margin,
    ];
    let targets = [target.origin, target.origin + cap(target.velocity) * t];
    for e in eyes {
        // Eyes predicted into a wall are not valid.
        if e != viewer.eye && !world.line_of_sight(viewer.eye, e) {
            continue;
        }
        for o in targets {
            for p in target_points(o, target.height, side, cfg.margin) {
                if world.line_of_sight(e, p) && !blocked(e, p) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_physics::brush::Brush;
    use engine_physics::material::MaterialId;

    fn wall_world() -> CollisionWorld {
        // A wall between x = -32..32 along z, height 200, at z = -500 .. -484.
        let b = Brush::from_box(
            engine_core::Aabb::new(
                Vec3::new(-300.0, 0.0, -516.0),
                Vec3::new(300.0, 200.0, -500.0),
            ),
            MaterialId(1),
        );
        CollisionWorld::new(vec![b])
    }

    #[test]
    fn hidden_behind_wall_visible_when_peeking() {
        let w = wall_world();
        let cfg = VisibilityConfig::default();
        let viewer = Viewer {
            eye: Vec3::new(0.0, 64.0, 0.0),
            velocity: Vec3::ZERO,
            delay: 0.03,
        };
        let hidden = Target {
            origin: Vec3::new(0.0, 0.0, -1000.0),
            velocity: Vec3::ZERO,
            height: 72.0,
        };
        assert!(!potentially_visible(&w, &cfg, &viewer, &hidden, &|_, _| {
            false
        }));
        // Target about to step out from behind the wall edge at 250 u/s.
        let peeker = Target {
            origin: Vec3::new(560.0, 0.0, -1000.0),
            velocity: Vec3::new(250.0, 0.0, 0.0),
            height: 72.0,
        };
        assert!(potentially_visible(&w, &cfg, &viewer, &peeker, &|_, _| {
            false
        }));
        // Fully visible target hidden by a smoke.
        let open = Target {
            origin: Vec3::new(1000.0, 0.0, -1000.0),
            velocity: Vec3::ZERO,
            height: 72.0,
        };
        assert!(potentially_visible(&w, &cfg, &viewer, &open, &|_, _| false));
        assert!(!potentially_visible(&w, &cfg, &viewer, &open, &|_, _| true));
    }
}
