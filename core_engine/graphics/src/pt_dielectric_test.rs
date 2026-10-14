//! Pure-Rust mirror of the dielectric transmission math in `pathtrace_tri_entry.wgsl`
//! (`pt_fresnel_dielectric` + the WGSL builtin `refract`). Verifies Snell's-law bending
//! and total-internal-reflection at the critical angle. Deterministic and headless — the
//! most valuable check per PT_TRANSMISSION_PLAN §7 (T1). If the shader math changes, these
//! constants must move in lockstep.

use glam::Vec3;

/// Exact unpolarized dielectric Fresnel reflectance — byte-for-byte the WGSL
/// `pt_fresnel_dielectric(cos_i, eta)`. `eta = ior_from / ior_to`. Returns 1.0 on TIR.
fn fresnel_dielectric(cos_i: f32, eta: f32) -> f32 {
    let s2t = eta * eta * (1.0 - cos_i * cos_i);
    if s2t >= 1.0 {
        return 1.0;
    }
    let cos_t = (1.0 - s2t).sqrt();
    let rs = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
    let rp = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
    (0.5 * (rs * rs + rp * rp)).clamp(0.0, 1.0)
}

/// WGSL `refract(I, N, eta)` semantics: I points INTO the surface, N opposes I,
/// returns vec3(0) on TIR (k < 0).
fn refract(i: Vec3, n: Vec3, eta: f32) -> Vec3 {
    let cosi = i.dot(n);
    let k = 1.0 - eta * eta * (1.0 - cosi * cosi);
    if k < 0.0 {
        Vec3::ZERO
    } else {
        eta * i - (eta * cosi + k.sqrt()) * n
    }
}

#[test]
fn snell_law_bend_air_to_glass() {
    // Air (1.0) -> glass (1.5). eta = 1/1.5. Interface normal +Y (outward, opposes ray).
    let ior = 1.5f32;
    let eta = 1.0 / ior;
    let nf = Vec3::Y; // outward normal, ray comes from above going down
    for deg in [10.0f32, 30.0, 45.0, 60.0, 80.0] {
        let theta_i = deg.to_radians();
        // Incident ray travelling into the surface (downward), incidence angle theta_i.
        let ray_d = Vec3::new(theta_i.sin(), -theta_i.cos(), 0.0).normalize();
        // nf must oppose the ray for refract(); flip to face the incoming ray.
        let n = if ray_d.dot(nf) < 0.0 { nf } else { -nf };
        let rd = refract(ray_d, n, eta).normalize();
        // Snell: n1 sin(theta_i) = n2 sin(theta_t).
        let expected_sin_t = (1.0 * theta_i.sin()) / ior;
        // Transmitted angle from the (inward) normal -n.
        let sin_t = rd.cross(-n).length();
        assert!(
            (sin_t - expected_sin_t).abs() < 1e-4,
            "Snell mismatch at {deg} deg: got sin_t={sin_t}, expected {expected_sin_t}"
        );
        // Bent ray must continue into the medium (below the interface).
        assert!(rd.y < 0.0, "refracted ray should go into medium at {deg} deg");
    }
}

#[test]
fn tir_fires_at_critical_angle_glass_to_air() {
    // Glass (1.5) -> air (1.0). eta = 1.5. Critical angle = asin(1/1.5) ≈ 41.81 deg.
    let eta = 1.5f32;
    let theta_c = (1.0f32 / 1.5).asin();
    let theta_c_deg = theta_c.to_degrees();
    assert!((theta_c_deg - 41.81).abs() < 0.1, "critical angle sanity");

    // Just BELOW critical -> transmits (F < 1).
    let below = (theta_c_deg - 2.0).to_radians();
    assert!(
        fresnel_dielectric(below.cos(), eta) < 1.0,
        "below critical angle must transmit (F<1)"
    );
    // Just ABOVE critical -> TIR (F == 1).
    let above = (theta_c_deg + 2.0).to_radians();
    assert_eq!(
        fresnel_dielectric(above.cos(), eta),
        1.0,
        "above critical angle must be TIR (F=1)"
    );
    // At exactly critical, s2t >= 1.0 -> TIR by our >= guard.
    assert_eq!(fresnel_dielectric(theta_c.cos(), eta), 1.0);
}

#[test]
fn fresnel_normal_incidence_matches_schlick_f0() {
    // At normal incidence dielectric Fresnel == ((n1-n2)/(n1+n2))^2.
    // Air->glass 1.5: f0 = (0.5/2.5)^2 = 0.04.
    let f = fresnel_dielectric(1.0, 1.0 / 1.5);
    assert!((f - 0.04).abs() < 1e-3, "normal-incidence F should be ~0.04, got {f}");
}

#[test]
fn fresnel_energy_conserving_no_gain() {
    // R must stay in [0,1] across all angles and both directions -> R + T = 1, no energy gain.
    for &eta in &[1.0 / 1.5f32, 1.5, 1.0 / 1.33, 1.33] {
        for i in 0..=90 {
            let cos_i = (i as f32).to_radians().cos();
            let r = fresnel_dielectric(cos_i, eta);
            assert!((0.0..=1.0).contains(&r), "F out of range: {r} at {i} deg eta {eta}");
        }
    }
}
