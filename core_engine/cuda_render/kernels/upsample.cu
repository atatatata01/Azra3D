// ============================================================
// upsample.cu — Catmull-Rom bicubic + CAS upsampling
// Port from WGSL compute.wgsl fn pass_upsample
// ============================================================

#include "common.cuh"

// Catmull-Rom spline weights
__device__ __forceinline__ float4 cr_weights(float t) {
    float t2 = t * t, t3 = t2 * t;
    return make_float4(
        -0.5f*t3 + t2 - 0.5f*t,
         1.5f*t3 - 2.5f*t2 + 1.0f,
        -1.5f*t3 + 2.0f*t2 + 0.5f*t,
         0.5f*t3 - 0.5f*t2
    );
}

extern "C" __global__ void upsample_kernel(
    const unsigned char* __restrict__ ldr_in,  // render-res RGBA8
    unsigned char*       __restrict__ ldr_out, // viewport-res RGBA8
    unsigned int in_w,
    unsigned int in_h,
    unsigned int out_w,
    unsigned int out_h
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= out_w || gy >= out_h) return;

    float2 uv = make_float2(
        ((float)gx + 0.5f) / (float)out_w,
        ((float)gy + 0.5f) / (float)out_h
    );
    float src_x = uv.x * (float)in_w - 0.5f;
    float src_y = uv.y * (float)in_h - 0.5f;
    int base_x = (int)floorf(src_x);
    int base_y = (int)floorf(src_y);
    float frac_x = src_x - floorf(src_x);
    float frac_y = src_y - floorf(src_y);
    int max_x = (int)in_w - 1;
    int max_y = (int)in_h - 1;

    float4 wx = cr_weights(frac_x);
    float4 wy = cr_weights(frac_y);

    auto fetch = [&](int x, int y) -> float3 {
        x = min(max(x, 0), max_x);
        y = min(max(y, 0), max_y);
        unsigned int idx = (y * in_w + x) * 4;
        return make_f3(ldr_in[idx] / 255.0f, ldr_in[idx+1] / 255.0f, ldr_in[idx+2] / 255.0f);
    };

    // 4×4 bicubic sampling
    float3 color = make_f3(0,0,0);
    float wy_arr[4] = {wy.x, wy.y, wy.z, wy.w};
    float wx_arr[4] = {wx.x, wx.y, wx.z, wx.w};
    for (int j = 0; j < 4; j++) {
        for (int i = 0; i < 4; i++) {
            float3 s = fetch(base_x + i - 1, base_y + j - 1);
            color = add3(color, mul3f(s, wx_arr[i] * wy_arr[j]));
        }
    }

    // Contrast-Adaptive Sharpening (CAS)
    int nx = min(max((int)roundf(src_x), 0), max_x);
    int ny = min(max((int)roundf(src_y), 0), max_y);
    float3 c  = fetch(nx, ny);
    float3 tn = fetch(nx, max(ny-1, 0));
    float3 ts = fetch(nx, min(ny+1, max_y));
    float3 te = fetch(min(nx+1, max_x), ny);
    float3 tw = fetch(max(nx-1, 0), ny);

    float3 mn = min3f(min3f(tn, ts), min3f(te, tw));
    float3 mx = max3(max3(tn, ts), max3(te, tw));
    float3 peak = make_f3(
        1.0f - __saturatef((mx.x - mn.x) * 4.0f),
        1.0f - __saturatef((mx.y - mn.y) * 4.0f),
        1.0f - __saturatef((mx.z - mn.z) * 4.0f)
    );
    float3 detail = sub3(c, mul3f(add3(add3(tn, ts), add3(te, tw)), 0.25f));
    color = make_f3(
        __saturatef(color.x + detail.x * peak.x * 0.5f),
        __saturatef(color.y + detail.y * peak.y * 0.5f),
        __saturatef(color.z + detail.z * peak.z * 0.5f)
    );

    unsigned int out_idx = (gy * out_w + gx) * 4;
    ldr_out[out_idx + 0] = (unsigned char)(color.x * 255.0f);
    ldr_out[out_idx + 1] = (unsigned char)(color.y * 255.0f);
    ldr_out[out_idx + 2] = (unsigned char)(color.z * 255.0f);
    ldr_out[out_idx + 3] = 255;
}
