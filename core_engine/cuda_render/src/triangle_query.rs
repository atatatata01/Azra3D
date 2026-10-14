use bytemuck::{Pod, Zeroable};

pub const OPTIX_MATERIAL_PARITY_READY: bool = false;
pub const OPTIX_MATERIAL_PARITY_GAP: &str = "OptiX traversal is not a production renderer: GPU-shared ray queues, WGSL material/light/HDR shading, synchronization and interaction scheduling are not connected";

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct TriangleRay {
    pub origin: [f32; 3],
    pub t_min: f32,
    pub direction: [f32; 3],
    pub t_max: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct TriangleIdentity {
    pub triangle: u32,
    pub instance: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct TriangleHit {
    pub distance: f32,
    pub barycentric_u: f32,
    pub barycentric_v: f32,
    pub triangle: u32,
    pub instance: u32,
    pub hit: u32,
    pub valid: u32,
    pub reserved: u32,
}

pub(crate) fn validate_triangle_scene(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
    tri_materials: &[u32],
    material_count: usize,
) -> Result<(), String> {
    if positions.len() > u32::MAX as usize || indices.len() / 3 > u32::MAX as usize {
        return Err("triangle geometry exceeds OptiX u32 limits".into());
    }
    if normals.len() != positions.len() {
        return Err("triangle normals and positions counts differ".into());
    }
    if indices.len() % 3 != 0 || tri_materials.len() != indices.len() / 3 {
        return Err("triangle index and material counts are inconsistent".into());
    }
    if positions.iter().chain(normals).flatten().any(|value| !value.is_finite()) {
        return Err("triangle positions and normals must be finite".into());
    }
    if indices.iter().any(|&index| index as usize >= positions.len()) {
        return Err("triangle vertex index out of bounds".into());
    }
    if tri_materials.iter().any(|&index| index as usize >= material_count) {
        return Err("triangle material index out of bounds".into());
    }
    Ok(())
}

#[cfg(feature = "cuda")]
unsafe impl cudarc::driver::DeviceRepr for TriangleRay {}
#[cfg(feature = "cuda")]
unsafe impl cudarc::driver::ValidAsZeroBits for TriangleRay {}
#[cfg(feature = "cuda")]
unsafe impl cudarc::driver::DeviceRepr for TriangleIdentity {}
#[cfg(feature = "cuda")]
unsafe impl cudarc::driver::ValidAsZeroBits for TriangleIdentity {}
#[cfg(feature = "cuda")]
unsafe impl cudarc::driver::DeviceRepr for TriangleHit {}
#[cfg(feature = "cuda")]
unsafe impl cudarc::driver::ValidAsZeroBits for TriangleHit {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn query_abi_matches_device_records() {
        assert_eq!(size_of::<TriangleRay>(), 32);
        assert_eq!(align_of::<TriangleRay>(), 4);
        assert_eq!(offset_of!(TriangleRay, t_min), 12);
        assert_eq!(offset_of!(TriangleRay, direction), 16);
        assert_eq!(offset_of!(TriangleRay, t_max), 28);
        assert_eq!(size_of::<TriangleIdentity>(), 8);
        assert_eq!(offset_of!(TriangleIdentity, instance), 4);
        assert_eq!(size_of::<TriangleHit>(), 32);
        assert_eq!(offset_of!(TriangleHit, triangle), 12);
        assert_eq!(offset_of!(TriangleHit, instance), 16);
        assert_eq!(offset_of!(TriangleHit, hit), 20);
        assert_eq!(offset_of!(TriangleHit, valid), 24);
    }

    #[test]
    fn traversal_does_not_claim_material_parity() {
        assert!(!OPTIX_MATERIAL_PARITY_READY);
        assert!(OPTIX_MATERIAL_PARITY_GAP.contains("not connected"));
    }

    #[test]
    fn rejects_invalid_geometry_before_gpu_upload() {
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let normals = [[0.0, 0.0, 1.0]; 3];
        assert!(validate_triangle_scene(&positions, &normals, &[0, 1, 2], &[0], 1).is_ok());
        assert!(validate_triangle_scene(&[], &[], &[], &[], 0).is_ok());
        assert!(validate_triangle_scene(&positions, &normals, &[0, 1], &[], 1).is_err());
        assert!(validate_triangle_scene(&positions, &normals, &[0, 1, 3], &[0], 1).is_err());
        assert!(validate_triangle_scene(&positions, &normals, &[0, 1, 2], &[1], 1).is_err());
        assert!(validate_triangle_scene(&positions, &normals, &[0, 1, 2], &[0], 0).is_err());
        assert!(validate_triangle_scene(&positions, &[], &[0, 1, 2], &[0], 1).is_err());
        let mut invalid = positions;
        invalid[0][0] = f32::NAN;
        assert!(validate_triangle_scene(&invalid, &normals, &[0, 1, 2], &[0], 1).is_err());
    }
}
