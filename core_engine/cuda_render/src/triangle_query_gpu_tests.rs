use crate::optix_accel::{HwRtMaterial, OptixTriangleAccel};
use crate::optix_tri_pipeline::OptixTriPipeline;
use crate::{TriangleHit, TriangleIdentity, TriangleRay};
use cudarc::driver::CudaDevice;

#[test]
#[ignore = "requires CUDA, NVIDIA RTX and OptiX runtime"]
fn optix_queries_preserve_hit_identity_barycentrics_and_segments() {
    let device = CudaDevice::new_with_stream(0).expect("CUDA non-default stream");
    let mut pipeline = OptixTriPipeline::try_new(&device).expect("OptiX triangle queries");
    let mut accel = OptixTriangleAccel::new(&device).unwrap();
    let positions = [
        [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0],
    ];
    accel.build(
        &device, pipeline.ctx_handle(), &positions, &[[0.0, 0.0, 1.0]; 6],
        &[0, 1, 2, 3, 4, 5], &[0, 0], &[HwRtMaterial::default()],
    ).unwrap();
    let identities = device.htod_copy(vec![
        TriangleIdentity { triangle: 73, instance: 9 },
        TriangleIdentity { triangle: 11, instance: 4 },
    ]).unwrap();
    let front = TriangleRay {
        origin: [0.25, 0.25, 2.0], t_min: 0.0,
        direction: [0.0, 0.0, -1.0], t_max: 10.0,
    };
    let host_rays = vec![
        front,
        TriangleRay { origin: [2.0, 2.0, 2.0], ..front },
        TriangleRay { t_max: 0.5, ..front },
        TriangleRay { t_min: 1.5, ..front },
        TriangleRay { origin: [0.25, 0.25, -1.0], direction: [0.0, 0.0, 1.0], ..front },
        TriangleRay { direction: [0.0, 0.0, -2.0], ..front },
        TriangleRay { direction: [0.0; 3], ..front },
        TriangleRay { origin: [f32::NAN, 0.0, 2.0], ..front },
    ];
    let rays = device.htod_copy(host_rays).unwrap();
    let mut hits = device.alloc_zeros::<TriangleHit>(8).unwrap();
    pipeline.launch_queries(&device, &accel, &rays, &mut hits, &identities).unwrap();
    let result = device.dtoh_sync_copy(&hits).unwrap();
    assert_eq!((result[0].hit, result[0].triangle, result[0].instance), (1, 11, 4));
    assert!((result[0].distance - 1.0).abs() < 1e-6);
    assert!((result[0].barycentric_u - 0.25).abs() < 1e-6);
    assert!((result[0].barycentric_v - 0.25).abs() < 1e-6);
    for index in [1, 2] {
        assert_eq!((result[index].valid, result[index].hit), (1, 0));
        assert_eq!(result[index].triangle, u32::MAX);
    }
    assert_eq!((result[3].hit, result[3].triangle, result[3].instance), (1, 73, 9));
    assert!((result[3].distance - 2.0).abs() < 1e-6);
    assert_eq!((result[4].hit, result[4].triangle), (1, 73));
    assert!((result[5].distance - 0.5).abs() < 1e-6);
    assert_eq!((result[6].valid, result[7].valid), (0, 0));
    let mut wrong_size = device.alloc_zeros::<TriangleHit>(1).unwrap();
    assert!(pipeline.launch_queries(&device, &accel, &rays, &mut wrong_size, &identities).is_err());
    let wrong_ids = device.htod_copy(vec![TriangleIdentity { triangle: 0, instance: 0 }]).unwrap();
    assert!(pipeline.launch_queries(&device, &accel, &rays, &mut hits, &wrong_ids).is_err());
    let other_device = CudaDevice::new_with_stream(0).unwrap();
    let other_rays = other_device.htod_copy(vec![front; 8]).unwrap();
    assert!(pipeline.launch_queries(&device, &accel, &other_rays, &mut hits, &identities).is_err());
    accel.build(&device, pipeline.ctx_handle(), &[], &[], &[], &[], &[]).unwrap();
    let no_ids = device.htod_copy(Vec::<TriangleIdentity>::new()).unwrap();
    pipeline.launch_queries(&device, &accel, &rays, &mut hits, &no_ids).unwrap();
    let cleared = device.dtoh_sync_copy(&hits).unwrap();
    assert_eq!((cleared[0].valid, cleared[0].hit), (1, 0));
    assert_eq!(cleared[0].triangle, u32::MAX);
    assert!(accel.build(
        &device, pipeline.ctx_handle(), &positions, &[[0.0, 0.0, 1.0]; 6],
        &[0, 1, 6], &[0], &[HwRtMaterial::default()],
    ).is_err());
    assert_eq!(accel.traversable(), 0);
    assert_eq!(accel.num_triangles(), 0);
}
