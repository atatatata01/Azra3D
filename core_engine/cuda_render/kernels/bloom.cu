// ============================================================
// bloom.cu — Bloom extract + separable Gaussian blur
// Port from WGSL compute.wgsl bloom_extract/bloom_blur_h/bloom_blur_v
// ============================================================

#include "common.cuh"

// ---- Bloom extract: HDR → quarter-res bright pixels ----
extern "C" __global__ void bloom_extract_kernel(
    const GpuUniforms* __restrict__ cfg,
    const float4*      __restrict__ hdr_buf,
    float4*            __restrict__ bloom_out,
    unsigned int hdr_w,
    unsigned int hdr_h,
    unsigned int bloom_w,
    unsigned int bloom_h
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= bloom_w || gy >= bloom_h) return;

    // Sample 2×2 block from HDR
    float sx = (float)gx * (float)hdr_w / (float)bloom_w;
    float sy = (float)gy * (float)hdr_h / (float)bloom_h;
    int bx = (int)sx, by = (int)sy;
    auto hdr_at = [&](int x, int y) -> float3 {
        x = min(max(x, 0), (int)hdr_w - 1);
        y = min(max(y, 0), (int)hdr_h - 1);
        float4 v = hdr_buf[y * hdr_w + x];
        return make_f3(v.x, v.y, v.z);
    };
    float3 c0 = hdr_at(bx, by);
    float3 c1 = hdr_at(bx+1, by);
    float3 c2 = hdr_at(bx, by+1);
    float3 c3 = hdr_at(bx+1, by+1);
    float3 avg = mul3f(add3(add3(c0, c1), add3(c2, c3)), 0.25f);

    // Soft threshold
    float lum = avg.x * 0.2126f + avg.y * 0.7152f + avg.z * 0.0722f;
    float soft = fmaxf(0.0f, lum - cfg->bloom_threshold);
    float contribution = soft / (soft + 1.0f);

    bloom_out[gy * bloom_w + gx] = make_float4(
        avg.x * contribution, avg.y * contribution, avg.z * contribution, 1.0f);
}

// ---- Separable Gaussian blur (9-tap, sigma ~2.5) ----
// dir_x/dir_y: (1,0) for horizontal, (0,1) for vertical
extern "C" __global__ void bloom_blur_kernel(
    const float4* __restrict__ bloom_in,
    float4*       __restrict__ bloom_out,
    unsigned int bloom_w,
    unsigned int bloom_h,
    int dir_x,
    int dir_y
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= bloom_w || gy >= bloom_h) return;

    const float w0 = 0.2270f, w1 = 0.1945f, w2 = 0.1216f, w3 = 0.0540f, w4 = 0.0162f;

    auto sample = [&](int ox, int oy) -> float3 {
        int sx = min(max((int)gx + ox, 0), (int)bloom_w - 1);
        int sy = min(max((int)gy + oy, 0), (int)bloom_h - 1);
        float4 v = bloom_in[sy * bloom_w + sx];
        return make_f3(v.x, v.y, v.z);
    };

    float3 color = mul3f(sample(0, 0), w0);
    color = add3(color, mul3f(sample(dir_x*1, dir_y*1), w1));
    color = add3(color, mul3f(sample(-dir_x*1, -dir_y*1), w1));
    color = add3(color, mul3f(sample(dir_x*2, dir_y*2), w2));
    color = add3(color, mul3f(sample(-dir_x*2, -dir_y*2), w2));
    color = add3(color, mul3f(sample(dir_x*3, dir_y*3), w3));
    color = add3(color, mul3f(sample(-dir_x*3, -dir_y*3), w3));
    color = add3(color, mul3f(sample(dir_x*4, dir_y*4), w4));
    color = add3(color, mul3f(sample(-dir_x*4, -dir_y*4), w4));

    bloom_out[gy * bloom_w + gx] = make_float4(color.x, color.y, color.z, 1.0f);
}
