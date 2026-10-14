// ============================================================
// tonemap.cu — HDR→LDR tonemap + bloom/SSR/fog composite
// Port from WGSL compute.wgsl fn pass_tonemap
// ============================================================

#include "common.cuh"

__device__ __forceinline__ float3 aces_narkowicz(float3 x) {
    const float a = 2.51f, b = 0.03f, c = 2.43f, d = 0.59f, e = 0.14f;
    return make_f3(
        __saturatef((x.x * (a * x.x + b)) / (x.x * (c * x.x + d) + e)),
        __saturatef((x.y * (a * x.y + b)) / (x.y * (c * x.y + d) + e)),
        __saturatef((x.z * (a * x.z + b)) / (x.z * (c * x.z + d) + e))
    );
}

extern "C" __global__ void tonemap_kernel(
    const GpuUniforms* __restrict__ cfg,
    const float4*      __restrict__ hdr_buf,
    const float4*      __restrict__ bloom_buf,
    const float4*      __restrict__ ssr_buf,
    const float4*      __restrict__ fog_buf,
    unsigned char*     __restrict__ ldr_out,
    unsigned int width,
    unsigned int height,
    unsigned int bloom_w,
    unsigned int bloom_h
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= width || gy >= height) return;
    unsigned int pixel = gy * width + gx;

    float4 hdr4 = hdr_buf[pixel];
    float3 hdr = make_f3(hdr4.x, hdr4.y, hdr4.z);

    // Bloom composite
    if (cfg->post_flags & 2u) {
        float bx = (float)gx * (float)bloom_w / (float)width;
        float by = (float)gy * (float)bloom_h / (float)height;
        int ix = (int)floorf(bx), iy = (int)floorf(by);
        float fx = bx - floorf(bx), fy = by - floorf(by);
        // Bilinear upsample from quarter-res bloom
        int bw = (int)bloom_w, bh = (int)bloom_h;
        auto bl = [&](int x, int y) -> float3 {
            x = min(max(x, 0), bw - 1);
            y = min(max(y, 0), bh - 1);
            float4 v = bloom_buf[y * bw + x];
            return make_f3(v.x, v.y, v.z);
        };
        float3 b00 = bl(ix, iy), b10 = bl(ix+1, iy);
        float3 b01 = bl(ix, iy+1), b11 = bl(ix+1, iy+1);
        float3 bloom = mix3(mix3(b00, b10, fx), mix3(b01, b11, fx), fy);
        hdr = add3(hdr, mul3f(bloom, cfg->bloom_intensity));
    }

    // Fog composite
    if (cfg->post_flags & 8u) {
        float4 fog = fog_buf[pixel];
        hdr = add3(mul3f(hdr, fog.w), make_f3(fog.x, fog.y, fog.z));
    }

    // SSR composite
    if (cfg->post_flags & 4u) {
        float4 ssr = ssr_buf[pixel];
        hdr = mix3(hdr, make_f3(ssr.x, ssr.y, ssr.z), ssr.w);
    }

    // Exposure
    hdr = mul3f(hdr, cfg->exposure);

    // ACES tonemap
    float3 ldr = aces_narkowicz(hdr);

    // Gamma
    float3 gamma = make_f3(powf(ldr.x, 1.0f/2.2f), powf(ldr.y, 1.0f/2.2f), powf(ldr.z, 1.0f/2.2f));

    // Write RGBA8
    unsigned int out_idx = pixel * 4;
    ldr_out[out_idx + 0] = (unsigned char)(__saturatef(gamma.x) * 255.0f);
    ldr_out[out_idx + 1] = (unsigned char)(__saturatef(gamma.y) * 255.0f);
    ldr_out[out_idx + 2] = (unsigned char)(__saturatef(gamma.z) * 255.0f);
    ldr_out[out_idx + 3] = 255;
}
