// MaterialOutput node — terminal **Master Material** (UI label).
// Socket indices are append-only (old graphs keep 0–5). Advanced block starts at MO_ADVANCED_START.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Master Material";

/// Base surface channels (always "core" — match Properties base PBR).
pub const BASE_COUNT: usize = 6;
/// First Advanced socket index (Height). Indices below are Base.
pub const ADVANCED_START: usize = 6;

pub fn inputs() -> Vec<Socket> {
    vec![
        // ── Base ──────────────────────────────────────────────
        Socket::color("Albedo", [0.66, 0.585, 0.53]),       // 0
        Socket::float("Roughness", 0.65),                   // 1
        Socket::float("Metallic", 0.0),                     // 2
        Socket::float("Opacity", 1.0),                      // 3
        Socket::color("Emission", [0.0, 0.0, 0.0]),         // 4
        // AO mask (idx 5) — artistic dirt/wear; lighting still applies geo AO once.
        Socket::float("Ambient Occlusion", 1.0),            // 5
        // ── Advanced (Properties Advanced parity) ─────────────
        Socket::float("Height", 0.0),                       // 6
        Socket::vec3("Normal", [0.0, 0.0, 1.0]),            // 7
        Socket::float("Clearcoat", 0.0),                    // 8
        Socket::float("Clearcoat Rough", 0.1),              // 9
        Socket::float("IOR", 1.5),                          // 10
        Socket::float("Sheen", 0.0),                        // 11
        Socket::float("Emission Intensity", 0.0),           // 12
        Socket::float("Normal Strength", 1.0),              // 13
        Socket::float("Sheen Tint", 0.5),                   // 14
        Socket::float("Anisotropy", 0.0),                   // 15
        Socket::float("Anisotropy Rot", 0.0),               // 16
        Socket::float("SSS Radius", 0.0),                   // 17
        Socket::color("SSS Color", [1.0, 0.2, 0.1]),        // 18
        Socket::color("Glass Tint", [0.0, 0.0, 0.0]),       // 19
        Socket::float("Displacement", 0.0),                 // 20
        Socket::float("Bump Strength", 0.05),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![]
}
