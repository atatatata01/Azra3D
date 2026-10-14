#include <optix_device.h>
#include <math.h>
#include <stddef.h>

struct TriangleRay {
    float3 origin;
    float t_min;
    float3 direction;
    float t_max;
};

struct TriangleIdentity {
    unsigned int triangle;
    unsigned int instance;
};

struct TriangleHit {
    float distance;
    float barycentric_u;
    float barycentric_v;
    unsigned int triangle;
    unsigned int instance;
    unsigned int hit;
    unsigned int valid;
    unsigned int reserved;
};

struct TriangleQueryParams {
    const TriangleRay* rays;
    TriangleHit* hits;
    const TriangleIdentity* identities;
    OptixTraversableHandle traversable;
    unsigned int ray_count;
    unsigned int reserved;
};

static_assert(sizeof(TriangleRay) == 32, "TriangleRay ABI");
static_assert(offsetof(TriangleRay, direction) == 16, "TriangleRay direction ABI");
static_assert(sizeof(TriangleIdentity) == 8, "TriangleIdentity ABI");
static_assert(sizeof(TriangleHit) == 32, "TriangleHit ABI");
static_assert(offsetof(TriangleHit, valid) == 24, "TriangleHit valid ABI");
static_assert(sizeof(TriangleQueryParams) == 40, "TriangleQueryParams ABI");

extern "C" __constant__ TriangleQueryParams params;

extern "C" __global__ void __closesthit__triangle_query() {
    const float2 barycentric = optixGetTriangleBarycentrics();
    optixSetPayload_0(optixGetPrimitiveIndex());
    optixSetPayload_1(__float_as_uint(optixGetRayTmax()));
    optixSetPayload_2(__float_as_uint(barycentric.x));
    optixSetPayload_3(__float_as_uint(barycentric.y));
}

extern "C" __global__ void __miss__triangle_query() {
    optixSetPayload_0(0xFFFFFFFFu);
}

extern "C" __global__ void __raygen__triangle_query() {
    const unsigned int ray_index = optixGetLaunchIndex().x;
    if (ray_index >= params.ray_count) return;
    const TriangleRay ray = params.rays[ray_index];
    TriangleHit result = {};
    result.distance = ray.t_max;
    result.triangle = 0xFFFFFFFFu;
    result.instance = 0xFFFFFFFFu;
    const bool valid = isfinite(ray.origin.x) && isfinite(ray.origin.y)
        && isfinite(ray.origin.z) && isfinite(ray.direction.x)
        && isfinite(ray.direction.y) && isfinite(ray.direction.z)
        && isfinite(ray.t_min) && isfinite(ray.t_max)
        && ray.t_min >= 0.0f && ray.t_max > ray.t_min
        && (ray.direction.x != 0.0f || ray.direction.y != 0.0f || ray.direction.z != 0.0f);
    result.valid = valid ? 1u : 0u;
    if (valid && params.traversable != 0) {
        unsigned int triangle = 0xFFFFFFFFu;
        unsigned int distance = __float_as_uint(ray.t_max);
        unsigned int barycentric_u = 0u;
        unsigned int barycentric_v = 0u;
        optixTrace(params.traversable, ray.origin, ray.direction,
            ray.t_min, ray.t_max, 0.0f, OptixVisibilityMask(255),
            OPTIX_RAY_FLAG_DISABLE_ANYHIT, 0, 1, 0,
            triangle, distance, barycentric_u, barycentric_v);
        if (triangle != 0xFFFFFFFFu) {
            const TriangleIdentity identity = params.identities[triangle];
            result.distance = __uint_as_float(distance);
            result.barycentric_u = __uint_as_float(barycentric_u);
            result.barycentric_v = __uint_as_float(barycentric_v);
            result.triangle = identity.triangle;
            result.instance = identity.instance;
            result.hit = 1u;
        }
    }
    params.hits[ray_index] = result;
}
