// core_engine/render/src/tests_poly_pick_concave.rs
// ============================================================
// Headless GPU probe: poly select-buffer element visibility at CONCAVE corners
// ============================================================
//
// Drives the REAL select-buffer path (`PickingPipeline::encode_poly_select` — the same
// encode `poly_select_gpu.rs` submits every settle) against a real device and reads the ID
// image back. Skips cleanly when no adapter exists (CI).
//
// The bug this pins (owner screenshot 2026-08-11, "in certain angles verts in a sharp
// concave shape are not visible or selectable"):
//
//   A vertex at the inside corner of a box is a strict LOCAL MAXIMUM of surface depth —
//   every direction along the surface leads TOWARD the camera. The element pass compared
//   its own depth against the 3×3 window MAX of the face depth, which assumes some
//   neighbouring texel is FARTHER than the element. At a concave corner none is: the face
//   depth sampled at the pixel CENTRE is already nearer than the exact vertex depth by up
//   to one pixel of the surface's depth slope, and widening the window cannot help (a
//   local maximum stays the maximum at any radius). The vertex lost its own visibility
//   test, so it neither drew (draw_poly.rs `vert_visible`) nor picked (pick.rs) —
//   one root cause, both symptoms.
//
// Guard test in the same file: a vertex genuinely behind an occluding quad must STAY
// invisible, so the slope tolerance can never be widened into "verts show through walls".

#![cfg(test)]

use crate::camera::CameraUniforms;
use crate::picking::{PickingPipeline, PolyPickVertex};
use glam::{Quat, Vec3};

const W: u32 = 512;
const H: u32 = 512;

fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    })).ok()?;
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("poly_pick_probe_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()
}

/// Camera looking from `eye` at `target` — expressed through the app's own orbit
/// constructor so the probe cannot drift from the shipping projection.
fn camera(eye: Vec3, target: Vec3) -> CameraUniforms {
    let off = eye - target;
    let dist = off.length();
    let orbit = Quat::from_rotation_arc(Vec3::Z, off / dist);
    CameraUniforms::from_orbit(
        orbit,
        dist,
        target,
        W as f32 / H as f32,
        45.0,
        dist * 0.05,
        dist * 100.0,
        false,
    )
}

/// Two triangles for quad `[a,b,c,d]`, all tagged `id`.
fn quad(out: &mut Vec<PolyPickVertex>, a: Vec3, b: Vec3, c: Vec3, d: Vec3, id: u32) {
    for p in [a, b, c, a, c, d] {
        out.push(PolyPickVertex { pos: [p.x, p.y, p.z], id });
    }
}

/// Render the select buffer for `faces` (triangle list) + `verts` (points) and return the
/// decoded w*h ID image. Edge range is empty here — verts are what this file is about.
fn render_ids(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cam: &CameraUniforms,
    faces: &[PolyPickVertex],
    verts: &[PolyPickVertex],
) -> Vec<u32> {
    let mut pick = PickingPipeline::new(device);
    pick.ensure_textures(device, W, H);
    pick.ensure_full_staging(device);
    pick.upload_camera(queue, cam);
    pick.rebuild_bind_group(device);
    pick.rebuild_poly_elem_bind_group(device);

    let mut all = faces.to_vec();
    all.extend_from_slice(verts);
    let vcount = pick.upload_poly_vertices(device, queue, &all);
    let nf = faces.len() as u32;

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("poly_pick_probe"),
    });
    pick.encode_poly_select(&mut enc, nf, nf, vcount);
    pick.copy_full(&mut enc);
    queue.submit(Some(enc.finish()));
    let ready = pick.start_full_readback();
    pick.blocking_readback_full(device, &ready)
        .expect("select-buffer readback")
}

fn count_id(ids: &[u32], id: u32) -> usize {
    ids.iter().filter(|&&v| v == id).count()
}

/// Inside corner of an open box: floor + two walls meeting at the origin, viewed from a
/// low 3/4 angle (the floor near-grazing) — the owner's screenshot reduced to its minimum.
/// The corner vertex is unoccluded, so its ID must be readable in the select buffer.
#[test]
fn concave_corner_vertex_survives_the_element_pass() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter — skipping");
        return;
    };

    let o = Vec3::ZERO; // the concave corner: floor + wall_x + wall_z all meet here
    let mut faces = Vec::new();
    // floor (y = 0)
    quad(&mut faces, o, Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 2.0), Vec3::new(0.0, 0.0, 2.0), 1);
    // wall on x = 0
    quad(&mut faces, o, Vec3::new(0.0, 0.0, 2.0), Vec3::new(0.0, 2.0, 2.0), Vec3::new(0.0, 2.0, 0.0), 2);
    // wall on z = 0
    quad(&mut faces, o, Vec3::new(0.0, 2.0, 0.0), Vec3::new(2.0, 2.0, 0.0), Vec3::new(2.0, 0.0, 0.0), 3);

    const CORNER_ID: u32 = 101;
    const FLOOR_FAR_ID: u32 = 102; // convex silhouette vertex — the control that always worked
    let verts = vec![
        PolyPickVertex { pos: [0.0, 0.0, 0.0], id: CORNER_ID },
        PolyPickVertex { pos: [2.0, 0.0, 2.0], id: FLOOR_FAR_ID },
    ];

    // Low, near-grazing 3/4 view: every direction along the surface from the corner leads
    // toward the camera, which is exactly what defeats a window-max reference.
    let eye = Vec3::new(4.9, 1.35, 4.9);
    let ids = render_ids(&device, &queue, &camera(eye, Vec3::new(0.9, 0.6, 0.9)), &faces, &verts);

    // Sanity: the scene actually rasterized.
    assert!(count_id(&ids, 1) > 100, "floor face missing from the select buffer");
    assert!(
        count_id(&ids, FLOOR_FAR_ID) > 0,
        "control vertex (convex, on the silhouette) missing — probe setup is wrong, not the bug"
    );

    assert!(
        count_id(&ids, CORNER_ID) > 0,
        "CONCAVE CORNER VERTEX IS UNSELECTABLE: it is unoccluded but wrote no id. Its depth is a \
         local MAXIMUM of the surface, so no neighbouring face texel is farther than it and the \
         window-max reference discards it."
    );
}

/// The guard on the fix: a vertex genuinely behind a nearer quad must NOT write its id.
/// Without this, "make concave corners visible" degenerates into see-through verts.
#[test]
fn occluded_vertex_stays_hidden() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter — skipping");
        return;
    };

    let mut faces = Vec::new();
    // far quad (z = 0), fully covered in screen space by…
    quad(
        &mut faces,
        Vec3::new(-1.0, -1.0, 0.0),
        Vec3::new(1.0, -1.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(-1.0, 1.0, 0.0),
        1,
    );
    // …the near quad (z = 2), bigger and closer to the eye on +z.
    quad(
        &mut faces,
        Vec3::new(-1.5, -1.5, 2.0),
        Vec3::new(1.5, -1.5, 2.0),
        Vec3::new(1.5, 1.5, 2.0),
        Vec3::new(-1.5, 1.5, 2.0),
        2,
    );

    const HIDDEN_ID: u32 = 201;
    const FRONT_ID: u32 = 202;
    let verts = vec![
        PolyPickVertex { pos: [1.0, 1.0, 0.0], id: HIDDEN_ID },
        PolyPickVertex { pos: [1.5, 1.5, 2.0], id: FRONT_ID },
    ];

    let ids = render_ids(
        &device,
        &queue,
        &camera(Vec3::new(0.0, 0.0, 8.0), Vec3::ZERO),
        &faces,
        &verts,
    );

    assert!(count_id(&ids, FRONT_ID) > 0, "front vertex missing — probe setup is wrong");
    assert_eq!(
        count_id(&ids, HIDDEN_ID),
        0,
        "a vertex behind a solid quad wrote its id — the element visibility test is see-through"
    );
}

/// The adversarial case for the slope term: a near-GRAZING occluder, where one pixel of
/// surface carries a large depth delta and a slope-derived tolerance is at its most
/// generous. A vertex buried under that floor must still stay hidden.
#[test]
fn vertex_under_a_grazing_floor_stays_hidden() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter — skipping");
        return;
    };

    let mut faces = Vec::new();
    quad(
        &mut faces,
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(8.0, 0.0, 0.0),
        Vec3::new(8.0, 0.0, 8.0),
        Vec3::new(0.0, 0.0, 8.0),
        1,
    );

    const BURIED_ID: u32 = 301;
    const ON_FLOOR_ID: u32 = 302;
    let verts = vec![
        PolyPickVertex { pos: [4.0, -0.4, 4.0], id: BURIED_ID },
        PolyPickVertex { pos: [4.0, 0.0, 4.0], id: ON_FLOOR_ID },
    ];

    // Eye 0.6 above a floor 8 units deep: the far half is only a few degrees off edge-on.
    let ids = render_ids(
        &device,
        &queue,
        &camera(Vec3::new(4.0, 0.6, 12.0), Vec3::new(4.0, 0.0, 4.0)),
        &faces,
        &verts,
    );

    assert!(
        count_id(&ids, ON_FLOOR_ID) > 0,
        "a vertex lying ON the grazing floor must remain visible"
    );
    assert_eq!(
        count_id(&ids, BURIED_ID),
        0,
        "a vertex buried under a grazing floor wrote its id — the slope tolerance is too wide"
    );
}
