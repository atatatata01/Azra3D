use super::NativeRtError;
use glam::Mat4;
use render::pt_accel::PtInstanceGpu;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeRtEpochs {
    pub geometry: u64,
    pub transforms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeInstance {
    pub model: [[f32; 4]; 4],
    pub tri_off: u32,
    pub vert_off: u32,
}

impl From<&PtInstanceGpu> for NativeInstance {
    fn from(instance: &PtInstanceGpu) -> Self {
        Self {
            model: instance.model,
            tri_off: instance.tri_off,
            vert_off: instance.vert_off,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GeometryRange {
    pub first_triangle: u32,
    pub triangle_count: u32,
    pub first_vertex: u32,
}

pub(super) struct SceneLayout {
    pub ranges: Vec<GeometryRange>,
    pub placements: Vec<([f32; 12], usize)>,
}

pub(super) fn instance_transform(model: [[f32; 4]; 4]) -> Result<[f32; 12], NativeRtError> {
    let matrix = Mat4::from_cols_array_2d(&model);
    if !matrix.is_finite()
        || matrix.row(3).to_array() != [0.0, 0.0, 0.0, 1.0]
        || !matrix.inverse().is_finite()
    {
        return Err(NativeRtError("native RT requires finite invertible affine transforms"));
    }
    let mut transform = [0.0; 12];
    for row in 0..3 {
        transform[row * 4..row * 4 + 4].copy_from_slice(&matrix.row(row).to_array());
    }
    Ok(transform)
}

pub(super) fn scene_layout(
    vertex_count: u32,
    triangle_count: u32,
    instances: &[NativeInstance],
) -> Result<SceneLayout, NativeRtError> {
    if instances.len() > 0x0100_0000 {
        return Err(NativeRtError("native RT instance IDs exceed 24 bits"));
    }
    let mut geometries = BTreeMap::new();
    for instance in instances {
        if instance.tri_off >= triangle_count || instance.vert_off >= vertex_count {
            return Err(NativeRtError("native RT instance geometry offset is out of range"));
        }
        if let Some(previous) = geometries.insert(instance.tri_off, instance.vert_off) {
            if previous != instance.vert_off {
                return Err(NativeRtError("shared triangle geometry has inconsistent vertex offsets"));
            }
        }
    }
    let offsets: Vec<_> = geometries.into_iter().collect();
    let mut ranges = Vec::with_capacity(offsets.len());
    for (index, &(first_triangle, first_vertex)) in offsets.iter().enumerate() {
        let end = offsets.get(index + 1).map_or(triangle_count, |next| next.0);
        let count = end - first_triangle;
        if count.checked_mul(3).is_none() {
            return Err(NativeRtError("native RT index count overflows u32"));
        }
        ranges.push(GeometryRange { first_triangle, triangle_count: count, first_vertex });
    }
    let placements = instances.iter().map(|instance| {
        let geometry = ranges.binary_search_by_key(&instance.tri_off, |range| range.first_triangle)
            .expect("instance range was collected above");
        Ok((instance_transform(instance.model)?, geometry))
    }).collect::<Result<Vec<_>, NativeRtError>>()?;
    Ok(SceneLayout { ranges, placements })
}
