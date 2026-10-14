// ============================================================
// optix_host.c — Minimal C wrapper for OptiX 7.x API
// Compiled by build.rs when optix feature is enabled.
// Exposes clean fl_optix_* functions for Rust FFI.
// ============================================================

#include <optix.h>
#include <optix_function_table_definition.h>
#include <optix_stubs.h>
#include <cuda.h>
#include <string.h>
#include <stdlib.h>

// ---- Init ----

int fl_optix_init(void) {
    return (int)optixInit();
}

int fl_optix_context_create(CUcontext cu_ctx, void** out_ctx) {
    OptixDeviceContextOptions options = {0};
    return (int)optixDeviceContextCreate(cu_ctx, &options, (OptixDeviceContext*)out_ctx);
}

void fl_optix_context_destroy(void* ctx) {
    if (ctx) optixDeviceContextDestroy((OptixDeviceContext)ctx);
}

// ---- Module + Pipeline ----

int fl_optix_create_module_and_pipeline(
    void* ctx,
    const char* ptx_src, unsigned int ptx_size,
    int num_payload_values,
    int num_attribute_values,
    const char* launch_params_name,
    const char* raygen_name,
    const char* miss_name,
    const char* closesthit_name,
    const char* intersection_name,
    unsigned int max_trace_depth,
    void** out_pipeline,
    void** out_raygen_pg,
    void** out_miss_pg,
    void** out_hitgroup_pg
) {
    OptixResult res;
    char log[2048];
    size_t log_size;

    // Module compile options
    OptixModuleCompileOptions module_opts = {0};
    module_opts.maxRegisterCount = OPTIX_COMPILE_DEFAULT_MAX_REGISTER_COUNT;
    module_opts.optLevel = OPTIX_COMPILE_OPTIMIZATION_DEFAULT;
    module_opts.debugLevel = OPTIX_COMPILE_DEBUG_LEVEL_NONE;

    // Pipeline compile options
    OptixPipelineCompileOptions pipeline_opts = {0};
    pipeline_opts.usesMotionBlur = 0;
    pipeline_opts.traversableGraphFlags = OPTIX_TRAVERSABLE_GRAPH_FLAG_ALLOW_SINGLE_GAS;
    pipeline_opts.numPayloadValues = num_payload_values;
    pipeline_opts.numAttributeValues = num_attribute_values;
    pipeline_opts.exceptionFlags = OPTIX_EXCEPTION_FLAG_NONE;
    pipeline_opts.pipelineLaunchParamsVariableName = launch_params_name;
    pipeline_opts.usesPrimitiveTypeFlags = OPTIX_PRIMITIVE_TYPE_FLAGS_CUSTOM;

    // Create module
    OptixModule module = NULL;
    log_size = sizeof(log);
    res = optixModuleCreate(
        (OptixDeviceContext)ctx,
        &module_opts, &pipeline_opts,
        ptx_src, (size_t)ptx_size,
        log, &log_size, &module);
    if (res != OPTIX_SUCCESS) return (int)res;

    // Program group options (empty)
    OptixProgramGroupOptions pg_opts = {0};

    // Raygen
    OptixProgramGroupDesc raygen_desc = {0};
    raygen_desc.kind = OPTIX_PROGRAM_GROUP_KIND_RAYGEN;
    raygen_desc.raygen.module = module;
    raygen_desc.raygen.entryFunctionName = raygen_name;
    log_size = sizeof(log);
    res = optixProgramGroupCreate(
        (OptixDeviceContext)ctx, &raygen_desc, 1, &pg_opts,
        log, &log_size, (OptixProgramGroup*)out_raygen_pg);
    if (res != OPTIX_SUCCESS) return (int)res;

    // Miss
    OptixProgramGroupDesc miss_desc = {0};
    miss_desc.kind = OPTIX_PROGRAM_GROUP_KIND_MISS;
    miss_desc.miss.module = module;
    miss_desc.miss.entryFunctionName = miss_name;
    log_size = sizeof(log);
    res = optixProgramGroupCreate(
        (OptixDeviceContext)ctx, &miss_desc, 1, &pg_opts,
        log, &log_size, (OptixProgramGroup*)out_miss_pg);
    if (res != OPTIX_SUCCESS) return (int)res;

    // Hitgroup (closesthit + custom intersection)
    OptixProgramGroupDesc hitgroup_desc = {0};
    hitgroup_desc.kind = OPTIX_PROGRAM_GROUP_KIND_HITGROUP;
    hitgroup_desc.hitgroup.moduleCH = module;
    hitgroup_desc.hitgroup.entryFunctionNameCH = closesthit_name;
    hitgroup_desc.hitgroup.moduleIS = module;
    hitgroup_desc.hitgroup.entryFunctionNameIS = intersection_name;
    hitgroup_desc.hitgroup.moduleAH = NULL;
    hitgroup_desc.hitgroup.entryFunctionNameAH = NULL;
    log_size = sizeof(log);
    res = optixProgramGroupCreate(
        (OptixDeviceContext)ctx, &hitgroup_desc, 1, &pg_opts,
        log, &log_size, (OptixProgramGroup*)out_hitgroup_pg);
    if (res != OPTIX_SUCCESS) return (int)res;

    // Link pipeline
    OptixPipelineLinkOptions link_opts = {0};
    link_opts.maxTraceDepth = max_trace_depth;

    OptixProgramGroup pgs[3] = {
        *(OptixProgramGroup*)out_raygen_pg,
        *(OptixProgramGroup*)out_miss_pg,
        *(OptixProgramGroup*)out_hitgroup_pg
    };

    log_size = sizeof(log);
    res = optixPipelineCreate(
        (OptixDeviceContext)ctx,
        &pipeline_opts, &link_opts,
        pgs, 3,
        log, &log_size,
        (OptixPipeline*)out_pipeline);
    if (res != OPTIX_SUCCESS) return (int)res;

    // Set stack sizes
    optixPipelineSetStackSize(
        *(OptixPipeline*)out_pipeline,
        2 * 1024, 2 * 1024, 2 * 1024, 1);

    return 0;
}

// ---- Acceleration Structure ----

int fl_optix_accel_buffer_sizes(
    void* ctx,
    unsigned int num_prims,
    unsigned long long* out_temp_size,
    unsigned long long* out_output_size
) {
    OptixAccelBuildOptions build_opts = {0};
    build_opts.buildFlags = OPTIX_BUILD_FLAG_PREFER_FAST_TRACE | OPTIX_BUILD_FLAG_ALLOW_UPDATE;
    build_opts.operation = OPTIX_BUILD_OPERATION_BUILD;

    unsigned int input_flags = OPTIX_GEOMETRY_FLAG_NONE;
    CUdeviceptr null_ptr = 0;

    OptixBuildInput build_input = {0};
    build_input.type = OPTIX_BUILD_INPUT_TYPE_CUSTOM_PRIMITIVES;
    build_input.customPrimitiveArray.aabbBuffers = &null_ptr;
    build_input.customPrimitiveArray.numPrimitives = num_prims;
    build_input.customPrimitiveArray.strideInBytes = 6 * sizeof(float);
    build_input.customPrimitiveArray.flags = &input_flags;
    build_input.customPrimitiveArray.numSbtRecords = 1;

    OptixAccelBufferSizes sizes;
    OptixResult res = optixAccelComputeMemoryUsage(
        (OptixDeviceContext)ctx, &build_opts, &build_input, 1, &sizes);

    if (res == OPTIX_SUCCESS) {
        *out_temp_size = (unsigned long long)sizes.tempSizeInBytes;
        *out_output_size = (unsigned long long)sizes.outputSizeInBytes;
    }
    return (int)res;
}

int fl_optix_build_gas(
    void* ctx,
    CUstream stream,
    CUdeviceptr aabbs_device, unsigned int num_prims,
    CUdeviceptr temp_buf, unsigned long long temp_size,
    CUdeviceptr output_buf, unsigned long long output_size,
    unsigned long long* out_handle
) {
    OptixAccelBuildOptions build_opts = {0};
    build_opts.buildFlags = OPTIX_BUILD_FLAG_PREFER_FAST_TRACE | OPTIX_BUILD_FLAG_ALLOW_UPDATE;
    build_opts.operation = OPTIX_BUILD_OPERATION_BUILD;

    unsigned int input_flags = OPTIX_GEOMETRY_FLAG_NONE;

    OptixBuildInput build_input = {0};
    build_input.type = OPTIX_BUILD_INPUT_TYPE_CUSTOM_PRIMITIVES;
    build_input.customPrimitiveArray.aabbBuffers = &aabbs_device;
    build_input.customPrimitiveArray.numPrimitives = num_prims;
    build_input.customPrimitiveArray.strideInBytes = 6 * sizeof(float);
    build_input.customPrimitiveArray.flags = &input_flags;
    build_input.customPrimitiveArray.numSbtRecords = 1;

    OptixTraversableHandle handle = 0;
    OptixResult res = optixAccelBuild(
        (OptixDeviceContext)ctx, stream,
        &build_opts, &build_input, 1,
        temp_buf, (size_t)temp_size,
        output_buf, (size_t)output_size,
        &handle, NULL, 0);

    *out_handle = (unsigned long long)handle;
    return (int)res;
}

// ---- Triangle GAS (hardware triangle intersection via RT cores) ----

int fl_optix_tri_accel_buffer_sizes(
    void* ctx,
    unsigned int num_vertices,
    unsigned int num_triangles,
    unsigned long long* out_temp_size,
    unsigned long long* out_output_size
) {
    (void)num_vertices;
    OptixAccelBuildOptions build_opts = {0};
    build_opts.buildFlags = OPTIX_BUILD_FLAG_PREFER_FAST_TRACE | OPTIX_BUILD_FLAG_ALLOW_COMPACTION;
    build_opts.operation = OPTIX_BUILD_OPERATION_BUILD;

    unsigned int input_flags = OPTIX_GEOMETRY_FLAG_NONE;
    CUdeviceptr null_ptr = 0;

    OptixBuildInput build_input = {0};
    build_input.type = OPTIX_BUILD_INPUT_TYPE_TRIANGLES;
    build_input.triangleArray.vertexBuffers = &null_ptr;
    build_input.triangleArray.numVertices = num_vertices;
    build_input.triangleArray.vertexFormat = OPTIX_VERTEX_FORMAT_FLOAT3;
    build_input.triangleArray.vertexStrideInBytes = sizeof(float) * 3;
    build_input.triangleArray.indexBuffer = null_ptr;
    build_input.triangleArray.numIndexTriplets = num_triangles;
    build_input.triangleArray.indexFormat = OPTIX_INDICES_FORMAT_UNSIGNED_INT3;
    build_input.triangleArray.indexStrideInBytes = sizeof(unsigned int) * 3;
    build_input.triangleArray.flags = &input_flags;
    build_input.triangleArray.numSbtRecords = 1;

    OptixAccelBufferSizes sizes;
    OptixResult res = optixAccelComputeMemoryUsage(
        (OptixDeviceContext)ctx, &build_opts, &build_input, 1, &sizes);

    if (res == OPTIX_SUCCESS) {
        *out_temp_size = (unsigned long long)sizes.tempSizeInBytes;
        *out_output_size = (unsigned long long)sizes.outputSizeInBytes;
    }
    return (int)res;
}

int fl_optix_build_triangle_gas(
    void* ctx,
    CUstream stream,
    CUdeviceptr vertices_device, unsigned int num_vertices,
    CUdeviceptr indices_device, unsigned int num_triangles,
    CUdeviceptr temp_buf, unsigned long long temp_size,
    CUdeviceptr output_buf, unsigned long long output_size,
    unsigned long long* out_handle
) {
    OptixAccelBuildOptions build_opts = {0};
    build_opts.buildFlags = OPTIX_BUILD_FLAG_PREFER_FAST_TRACE | OPTIX_BUILD_FLAG_ALLOW_COMPACTION;
    build_opts.operation = OPTIX_BUILD_OPERATION_BUILD;

    unsigned int input_flags = OPTIX_GEOMETRY_FLAG_NONE;

    OptixBuildInput build_input = {0};
    build_input.type = OPTIX_BUILD_INPUT_TYPE_TRIANGLES;
    build_input.triangleArray.vertexBuffers = &vertices_device;
    build_input.triangleArray.numVertices = num_vertices;
    build_input.triangleArray.vertexFormat = OPTIX_VERTEX_FORMAT_FLOAT3;
    build_input.triangleArray.vertexStrideInBytes = sizeof(float) * 3;
    build_input.triangleArray.indexBuffer = indices_device;
    build_input.triangleArray.numIndexTriplets = num_triangles;
    build_input.triangleArray.indexFormat = OPTIX_INDICES_FORMAT_UNSIGNED_INT3;
    build_input.triangleArray.indexStrideInBytes = sizeof(unsigned int) * 3;
    build_input.triangleArray.flags = &input_flags;
    build_input.triangleArray.numSbtRecords = 1;

    OptixTraversableHandle handle = 0;
    OptixResult res = optixAccelBuild(
        (OptixDeviceContext)ctx, stream,
        &build_opts, &build_input, 1,
        temp_buf, (size_t)temp_size,
        output_buf, (size_t)output_size,
        &handle, NULL, 0);

    *out_handle = (unsigned long long)handle;
    return (int)res;
}

// ---- Triangle pipeline (built-in triangle IS, no custom intersection) ----

int fl_optix_create_triangle_module_and_pipeline(
    void* ctx,
    const char* ptx_src, unsigned int ptx_size,
    int num_payload_values,
    int num_attribute_values,
    const char* launch_params_name,
    const char* raygen_name,
    const char* miss_name,
    const char* closesthit_name,
    unsigned int max_trace_depth,
    void** out_pipeline,
    void** out_raygen_pg,
    void** out_miss_pg,
    void** out_hitgroup_pg
) {
    OptixResult res;
    char log[2048];
    size_t log_size;

    OptixModuleCompileOptions module_opts = {0};
    module_opts.maxRegisterCount = OPTIX_COMPILE_DEFAULT_MAX_REGISTER_COUNT;
    module_opts.optLevel = OPTIX_COMPILE_OPTIMIZATION_DEFAULT;
    module_opts.debugLevel = OPTIX_COMPILE_DEBUG_LEVEL_NONE;

    OptixPipelineCompileOptions pipeline_opts = {0};
    pipeline_opts.usesMotionBlur = 0;
    pipeline_opts.traversableGraphFlags = OPTIX_TRAVERSABLE_GRAPH_FLAG_ALLOW_SINGLE_GAS;
    pipeline_opts.numPayloadValues = num_payload_values;
    pipeline_opts.numAttributeValues = num_attribute_values;
    pipeline_opts.exceptionFlags = OPTIX_EXCEPTION_FLAG_NONE;
    pipeline_opts.pipelineLaunchParamsVariableName = launch_params_name;
    pipeline_opts.usesPrimitiveTypeFlags = OPTIX_PRIMITIVE_TYPE_FLAGS_TRIANGLE;

    OptixModule module = NULL;
    log_size = sizeof(log);
    res = optixModuleCreate(
        (OptixDeviceContext)ctx,
        &module_opts, &pipeline_opts,
        ptx_src, (size_t)ptx_size,
        log, &log_size, &module);
    if (res != OPTIX_SUCCESS) return (int)res;

    OptixProgramGroupOptions pg_opts = {0};

    OptixProgramGroupDesc raygen_desc = {0};
    raygen_desc.kind = OPTIX_PROGRAM_GROUP_KIND_RAYGEN;
    raygen_desc.raygen.module = module;
    raygen_desc.raygen.entryFunctionName = raygen_name;
    log_size = sizeof(log);
    res = optixProgramGroupCreate(
        (OptixDeviceContext)ctx, &raygen_desc, 1, &pg_opts,
        log, &log_size, (OptixProgramGroup*)out_raygen_pg);
    if (res != OPTIX_SUCCESS) return (int)res;

    OptixProgramGroupDesc miss_desc = {0};
    miss_desc.kind = OPTIX_PROGRAM_GROUP_KIND_MISS;
    miss_desc.miss.module = module;
    miss_desc.miss.entryFunctionName = miss_name;
    log_size = sizeof(log);
    res = optixProgramGroupCreate(
        (OptixDeviceContext)ctx, &miss_desc, 1, &pg_opts,
        log, &log_size, (OptixProgramGroup*)out_miss_pg);
    if (res != OPTIX_SUCCESS) return (int)res;

    // Built-in triangle intersection — only closest-hit in the hitgroup
    OptixProgramGroupDesc hitgroup_desc = {0};
    hitgroup_desc.kind = OPTIX_PROGRAM_GROUP_KIND_HITGROUP;
    hitgroup_desc.hitgroup.moduleCH = module;
    hitgroup_desc.hitgroup.entryFunctionNameCH = closesthit_name;
    hitgroup_desc.hitgroup.moduleIS = NULL;
    hitgroup_desc.hitgroup.entryFunctionNameIS = NULL;
    hitgroup_desc.hitgroup.moduleAH = NULL;
    hitgroup_desc.hitgroup.entryFunctionNameAH = NULL;
    log_size = sizeof(log);
    res = optixProgramGroupCreate(
        (OptixDeviceContext)ctx, &hitgroup_desc, 1, &pg_opts,
        log, &log_size, (OptixProgramGroup*)out_hitgroup_pg);
    if (res != OPTIX_SUCCESS) return (int)res;

    OptixPipelineLinkOptions link_opts = {0};
    link_opts.maxTraceDepth = max_trace_depth;

    OptixProgramGroup pgs[3] = {
        *(OptixProgramGroup*)out_raygen_pg,
        *(OptixProgramGroup*)out_miss_pg,
        *(OptixProgramGroup*)out_hitgroup_pg
    };

    log_size = sizeof(log);
    res = optixPipelineCreate(
        (OptixDeviceContext)ctx,
        &pipeline_opts, &link_opts,
        pgs, 3,
        log, &log_size,
        (OptixPipeline*)out_pipeline);
    if (res != OPTIX_SUCCESS) return (int)res;

    // Multi-bounce path tracing needs a deeper continuum stack than primary-only SDF.
    optixPipelineSetStackSize(
        *(OptixPipeline*)out_pipeline,
        2 * 1024, 2 * 1024, 2 * 1024, 1);

    return 0;
}

// ---- Launch ----

int fl_optix_launch(
    void* pipeline,
    CUstream stream,
    CUdeviceptr launch_params, unsigned long long params_size,
    CUdeviceptr raygen_sbt, unsigned int raygen_sbt_size,
    CUdeviceptr miss_sbt, unsigned int miss_stride, unsigned int miss_count,
    CUdeviceptr hitgroup_sbt, unsigned int hitgroup_stride, unsigned int hitgroup_count,
    unsigned int width, unsigned int height
) {
    OptixShaderBindingTable sbt = {0};
    sbt.raygenRecord = raygen_sbt;
    sbt.missRecordBase = miss_sbt;
    sbt.missRecordStrideInBytes = miss_stride;
    sbt.missRecordCount = miss_count;
    sbt.hitgroupRecordBase = hitgroup_sbt;
    sbt.hitgroupRecordStrideInBytes = hitgroup_stride;
    sbt.hitgroupRecordCount = hitgroup_count;

    return (int)optixLaunch(
        (OptixPipeline)pipeline, stream,
        launch_params, (size_t)params_size,
        &sbt, width, height, 1);
}

// ---- SBT record helpers ----

unsigned int fl_optix_sbt_header_size(void) {
    return OPTIX_SBT_RECORD_HEADER_SIZE;
}

int fl_optix_pack_sbt_header(void* program_group, void* out_header) {
    return (int)optixSbtRecordPackHeader(
        (OptixProgramGroup)program_group, out_header);
}
