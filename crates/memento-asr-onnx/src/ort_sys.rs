// @generated
//! Hand-verified Rust transcription of the first 153 fields of the
//! ONNX Runtime 1.22.0 `OrtApi` vtable (`onnxruntime_c_api.h`), plus
//! `OrtApiBase` and the `OrtGetApiBase` entry point.
//!
//! Generation method: the 1.22.0 header (from the same ONNX Runtime
//! install the C++ build links, `extern/CMakeLists.txt`) was run through
//! `gcc -E -P`, the `struct OrtApi` body was split field by field, and
//! each C declaration was mechanically mapped to an
//! `unsafe extern "system" fn` field. Field order and count match the
//! published struct exactly through `SessionOptionsAppendExecutionProvider_CUDA`
//! (index 152); the published struct is append-only, so a declared prefix
//! is ABI-compatible. Pointer constness is cosmetic (bindgen convention);
//! all pointee types are ABI-identical.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::os::raw::{c_char, c_int, c_void};

/// Generic placeholder for C function-pointer typedef parameters that the
/// engine never invokes.
pub type OrtCallback = Option<unsafe extern "system" fn()>;

/// CUDA provider options (legacy struct; layout copied from
/// `onnxruntime_c_api.h` 1.22.0). `OrtApi::SessionOptionsAppendExecutionProvider_CUDA`
/// consumes a pointer to this struct.
#[repr(C)]
pub struct OrtCUDAProviderOptions {
    /// CUDA device id. C++ default 0 (overridden by the engine).
    pub device_id: c_int,
    /// `OrtCudnnConvAlgoSearch`; C++ default `Exhaustive` (0).
    pub cudnn_conv_algo_search: c_int,
    /// C++ default `SIZE_MAX` (all memory).
    pub gpu_mem_limit: usize,
    /// C++ default 0 (kNextPowerOfTwo).
    pub arena_extend_strategy: c_int,
    /// C++ default 1.
    pub do_copy_in_default_stream: c_int,
    /// C++ default 0.
    pub has_user_compute_stream: c_int,
    pub user_compute_stream: *mut c_void,
    pub default_memory_arena_cfg: *mut OrtArenaCfg,
    pub tunable_op_enable: c_int,
    pub tunable_op_tuning_enable: c_int,
    pub tunable_op_max_tuning_duration_ms: c_int,
}

impl Default for OrtCUDAProviderOptions {
    /// The C++ `OrtCUDAProviderOptions{}` member initializers (`SIZE_MAX`
    /// memory limit, `Exhaustive` search, copy on default stream).
    fn default() -> Self {
        Self {
            device_id: 0,
            cudnn_conv_algo_search: 0,
            gpu_mem_limit: usize::MAX,
            arena_extend_strategy: 0,
            do_copy_in_default_stream: 1,
            has_user_compute_stream: 0,
            user_compute_stream: std::ptr::null_mut(),
            default_memory_arena_cfg: std::ptr::null_mut(),
            tunable_op_enable: 0,
            tunable_op_tuning_enable: 0,
            tunable_op_max_tuning_duration_ms: 0,
        }
    }
}

#[repr(C)] pub struct OrtAllocator { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtArenaCfg { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtCustomOp { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtCustomOpDomain { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtEnv { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtIoBinding { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtKernelContext { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtKernelInfo { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtMapTypeInfo { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtMemoryInfo { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtModelMetadata { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtRunOptions { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtSequenceTypeInfo { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtSession { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtSessionOptions { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtStatus { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtTensorTypeAndShapeInfo { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtThreadingOptions { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtTypeInfo { _opaque: [u8; 0] }
#[repr(C)] pub struct OrtValue { _opaque: [u8; 0] }

/// The ONNX Runtime C API vtable (published struct prefix).
#[repr(C)]
pub struct OrtApi {
    pub CreateStatus: unsafe extern "system" fn(c_int, *const c_char) -> *mut OrtStatus,
    pub GetErrorCode: unsafe extern "system" fn(*const OrtStatus) -> c_int,
    pub GetErrorMessage: unsafe extern "system" fn(*const OrtStatus) -> *const c_char,
    pub CreateEnv: unsafe extern "system" fn(c_int, *const c_char, *mut *mut OrtEnv) -> *mut OrtStatus,
    pub CreateEnvWithCustomLogger: unsafe extern "system" fn(OrtCallback, *mut c_void, c_int, *const c_char, *mut *mut OrtEnv) -> *mut OrtStatus,
    pub EnableTelemetryEvents: unsafe extern "system" fn(*const OrtEnv) -> *mut OrtStatus,
    pub DisableTelemetryEvents: unsafe extern "system" fn(*const OrtEnv) -> *mut OrtStatus,
    pub CreateSession: unsafe extern "system" fn(*const OrtEnv, *const c_char, *const OrtSessionOptions, *mut *mut OrtSession) -> *mut OrtStatus,
    pub CreateSessionFromArray: unsafe extern "system" fn(*const OrtEnv, *const c_void, usize, *const OrtSessionOptions, *mut *mut OrtSession) -> *mut OrtStatus,
    pub Run: unsafe extern "system" fn(*mut OrtSession, *const OrtRunOptions, *const *const c_char, *const *const OrtValue, usize, *const *const c_char, usize, *mut *mut OrtValue) -> *mut OrtStatus,
    pub CreateSessionOptions: unsafe extern "system" fn(*mut *mut OrtSessionOptions) -> *mut OrtStatus,
    pub SetOptimizedModelFilePath: unsafe extern "system" fn(*mut OrtSessionOptions, *const c_char) -> *mut OrtStatus,
    pub CloneSessionOptions: unsafe extern "system" fn(*const OrtSessionOptions, *mut *mut OrtSessionOptions) -> *mut OrtStatus,
    pub SetSessionExecutionMode: unsafe extern "system" fn(*mut OrtSessionOptions, c_int) -> *mut OrtStatus,
    pub EnableProfiling: unsafe extern "system" fn(*mut OrtSessionOptions, *const c_char) -> *mut OrtStatus,
    pub DisableProfiling: unsafe extern "system" fn(*mut OrtSessionOptions) -> *mut OrtStatus,
    pub EnableMemPattern: unsafe extern "system" fn(*mut OrtSessionOptions) -> *mut OrtStatus,
    pub DisableMemPattern: unsafe extern "system" fn(*mut OrtSessionOptions) -> *mut OrtStatus,
    pub EnableCpuMemArena: unsafe extern "system" fn(*mut OrtSessionOptions) -> *mut OrtStatus,
    pub DisableCpuMemArena: unsafe extern "system" fn(*mut OrtSessionOptions) -> *mut OrtStatus,
    pub SetSessionLogId: unsafe extern "system" fn(*mut OrtSessionOptions, *const c_char) -> *mut OrtStatus,
    pub SetSessionLogVerbosityLevel: unsafe extern "system" fn(*mut OrtSessionOptions, c_int) -> *mut OrtStatus,
    pub SetSessionLogSeverityLevel: unsafe extern "system" fn(*mut OrtSessionOptions, c_int) -> *mut OrtStatus,
    pub SetSessionGraphOptimizationLevel: unsafe extern "system" fn(*mut OrtSessionOptions, c_int) -> *mut OrtStatus,
    pub SetIntraOpNumThreads: unsafe extern "system" fn(*mut OrtSessionOptions, c_int) -> *mut OrtStatus,
    pub SetInterOpNumThreads: unsafe extern "system" fn(*mut OrtSessionOptions, c_int) -> *mut OrtStatus,
    pub CreateCustomOpDomain: unsafe extern "system" fn(*const c_char, *mut *mut OrtCustomOpDomain) -> *mut OrtStatus,
    pub CustomOpDomain_Add: unsafe extern "system" fn(*mut OrtCustomOpDomain, *const OrtCustomOp) -> *mut OrtStatus,
    pub AddCustomOpDomain: unsafe extern "system" fn(*mut OrtSessionOptions, *mut OrtCustomOpDomain) -> *mut OrtStatus,
    pub RegisterCustomOpsLibrary: unsafe extern "system" fn(*mut OrtSessionOptions, *const c_char, *mut *mut c_void) -> *mut OrtStatus,
    pub SessionGetInputCount: unsafe extern "system" fn(*const OrtSession, *mut usize) -> *mut OrtStatus,
    pub SessionGetOutputCount: unsafe extern "system" fn(*const OrtSession, *mut usize) -> *mut OrtStatus,
    pub SessionGetOverridableInitializerCount: unsafe extern "system" fn(*const OrtSession, *mut usize) -> *mut OrtStatus,
    pub SessionGetInputTypeInfo: unsafe extern "system" fn(*const OrtSession, usize, *mut *mut OrtTypeInfo) -> *mut OrtStatus,
    pub SessionGetOutputTypeInfo: unsafe extern "system" fn(*const OrtSession, usize, *mut *mut OrtTypeInfo) -> *mut OrtStatus,
    pub SessionGetOverridableInitializerTypeInfo: unsafe extern "system" fn(*const OrtSession, usize, *mut *mut OrtTypeInfo) -> *mut OrtStatus,
    pub SessionGetInputName: unsafe extern "system" fn(*const OrtSession, usize, *mut OrtAllocator, *mut *mut c_char) -> *mut OrtStatus,
    pub SessionGetOutputName: unsafe extern "system" fn(*const OrtSession, usize, *mut OrtAllocator, *mut *mut c_char) -> *mut OrtStatus,
    pub SessionGetOverridableInitializerName: unsafe extern "system" fn(*const OrtSession, usize, *mut OrtAllocator, *mut *mut c_char) -> *mut OrtStatus,
    pub CreateRunOptions: unsafe extern "system" fn(*mut *mut OrtRunOptions) -> *mut OrtStatus,
    pub RunOptionsSetRunLogVerbosityLevel: unsafe extern "system" fn(*mut OrtRunOptions, c_int) -> *mut OrtStatus,
    pub RunOptionsSetRunLogSeverityLevel: unsafe extern "system" fn(*mut OrtRunOptions, c_int) -> *mut OrtStatus,
    pub RunOptionsSetRunTag: unsafe extern "system" fn(*mut OrtRunOptions, *const c_char) -> *mut OrtStatus,
    pub RunOptionsGetRunLogVerbosityLevel: unsafe extern "system" fn(*const OrtRunOptions, *mut c_int) -> *mut OrtStatus,
    pub RunOptionsGetRunLogSeverityLevel: unsafe extern "system" fn(*const OrtRunOptions, *mut c_int) -> *mut OrtStatus,
    pub RunOptionsGetRunTag: unsafe extern "system" fn(*const OrtRunOptions, *mut *const c_char) -> *mut OrtStatus,
    pub RunOptionsSetTerminate: unsafe extern "system" fn(*mut OrtRunOptions) -> *mut OrtStatus,
    pub RunOptionsUnsetTerminate: unsafe extern "system" fn(*mut OrtRunOptions) -> *mut OrtStatus,
    pub CreateTensorAsOrtValue: unsafe extern "system" fn(*mut OrtAllocator, *const i64, usize, c_int, *mut *mut OrtValue) -> *mut OrtStatus,
    pub CreateTensorWithDataAsOrtValue: unsafe extern "system" fn(*const OrtMemoryInfo, *mut c_void, usize, *const i64, usize, c_int, *mut *mut OrtValue) -> *mut OrtStatus,
    pub IsTensor: unsafe extern "system" fn(*const OrtValue, *mut c_int) -> *mut OrtStatus,
    pub GetTensorMutableData: unsafe extern "system" fn(*mut OrtValue, *mut *mut c_void) -> *mut OrtStatus,
    pub FillStringTensor: unsafe extern "system" fn(*mut OrtValue, *const *const c_char, usize) -> *mut OrtStatus,
    pub GetStringTensorDataLength: unsafe extern "system" fn(*const OrtValue, *mut usize) -> *mut OrtStatus,
    pub GetStringTensorContent: unsafe extern "system" fn(*const OrtValue, *mut c_void, usize, *mut usize, usize) -> *mut OrtStatus,
    pub CastTypeInfoToTensorInfo: unsafe extern "system" fn(*const OrtTypeInfo, *mut *const OrtTensorTypeAndShapeInfo) -> *mut OrtStatus,
    pub GetOnnxTypeFromTypeInfo: unsafe extern "system" fn(*const OrtTypeInfo, *mut c_int) -> *mut OrtStatus,
    pub CreateTensorTypeAndShapeInfo: unsafe extern "system" fn(*mut *mut OrtTensorTypeAndShapeInfo) -> *mut OrtStatus,
    pub SetTensorElementType: unsafe extern "system" fn(*mut OrtTensorTypeAndShapeInfo, c_int) -> *mut OrtStatus,
    pub SetDimensions: unsafe extern "system" fn(*mut OrtTensorTypeAndShapeInfo, *const i64, usize) -> *mut OrtStatus,
    pub GetTensorElementType: unsafe extern "system" fn(*const OrtTensorTypeAndShapeInfo, *mut c_int) -> *mut OrtStatus,
    pub GetDimensionsCount: unsafe extern "system" fn(*const OrtTensorTypeAndShapeInfo, *mut usize) -> *mut OrtStatus,
    pub GetDimensions: unsafe extern "system" fn(*const OrtTensorTypeAndShapeInfo, *mut i64, usize) -> *mut OrtStatus,
    pub GetSymbolicDimensions: unsafe extern "system" fn(*const OrtTensorTypeAndShapeInfo, *mut *const c_char, usize) -> *mut OrtStatus,
    pub GetTensorShapeElementCount: unsafe extern "system" fn(*const OrtTensorTypeAndShapeInfo, *mut usize) -> *mut OrtStatus,
    pub GetTensorTypeAndShape: unsafe extern "system" fn(*const OrtValue, *mut *mut OrtTensorTypeAndShapeInfo) -> *mut OrtStatus,
    pub GetTypeInfo: unsafe extern "system" fn(*const OrtValue, *mut *mut OrtTypeInfo) -> *mut OrtStatus,
    pub GetValueType: unsafe extern "system" fn(*const OrtValue, *mut c_int) -> *mut OrtStatus,
    pub CreateMemoryInfo: unsafe extern "system" fn(*const c_char, c_int, c_int, c_int, *mut *mut OrtMemoryInfo) -> *mut OrtStatus,
    pub CreateCpuMemoryInfo: unsafe extern "system" fn(c_int, c_int, *mut *mut OrtMemoryInfo) -> *mut OrtStatus,
    pub CompareMemoryInfo: unsafe extern "system" fn(*const OrtMemoryInfo, *const OrtMemoryInfo, *mut c_int) -> *mut OrtStatus,
    pub MemoryInfoGetName: unsafe extern "system" fn(*const OrtMemoryInfo, *mut *const c_char) -> *mut OrtStatus,
    pub MemoryInfoGetId: unsafe extern "system" fn(*const OrtMemoryInfo, *mut c_int) -> *mut OrtStatus,
    pub MemoryInfoGetMemType: unsafe extern "system" fn(*const OrtMemoryInfo, *mut c_int) -> *mut OrtStatus,
    pub MemoryInfoGetType: unsafe extern "system" fn(*const OrtMemoryInfo, *mut c_int) -> *mut OrtStatus,
    pub AllocatorAlloc: unsafe extern "system" fn(*mut OrtAllocator, usize, *mut *mut c_void) -> *mut OrtStatus,
    pub AllocatorFree: unsafe extern "system" fn(*mut OrtAllocator, *mut c_void) -> *mut OrtStatus,
    pub AllocatorGetInfo: unsafe extern "system" fn(*const OrtAllocator, *mut *const OrtMemoryInfo) -> *mut OrtStatus,
    pub GetAllocatorWithDefaultOptions: unsafe extern "system" fn(*mut *mut OrtAllocator) -> *mut OrtStatus,
    pub AddFreeDimensionOverride: unsafe extern "system" fn(*mut OrtSessionOptions, *const c_char, i64) -> *mut OrtStatus,
    pub GetValue: unsafe extern "system" fn(*const OrtValue, c_int, *mut OrtAllocator, *mut *mut OrtValue) -> *mut OrtStatus,
    pub GetValueCount: unsafe extern "system" fn(*const OrtValue, *mut usize) -> *mut OrtStatus,
    pub CreateValue: unsafe extern "system" fn(*const *const OrtValue, usize, c_int, *mut *mut OrtValue) -> *mut OrtStatus,
    pub CreateOpaqueValue: unsafe extern "system" fn(*const c_char, *const c_char, *const c_void, usize, *mut *mut OrtValue) -> *mut OrtStatus,
    pub GetOpaqueValue: unsafe extern "system" fn(*const c_char, *const c_char, *const OrtValue, *mut c_void, usize) -> *mut OrtStatus,
    pub KernelInfoGetAttribute_float: unsafe extern "system" fn(*const OrtKernelInfo, *const c_char, *mut f32) -> *mut OrtStatus,
    pub KernelInfoGetAttribute_int64: unsafe extern "system" fn(*const OrtKernelInfo, *const c_char, *mut i64) -> *mut OrtStatus,
    pub KernelInfoGetAttribute_string: unsafe extern "system" fn(*const OrtKernelInfo, *const c_char, *mut c_char, *mut usize) -> *mut OrtStatus,
    pub KernelContext_GetInputCount: unsafe extern "system" fn(*const OrtKernelContext, *mut usize) -> *mut OrtStatus,
    pub KernelContext_GetOutputCount: unsafe extern "system" fn(*const OrtKernelContext, *mut usize) -> *mut OrtStatus,
    pub KernelContext_GetInput: unsafe extern "system" fn(*const OrtKernelContext, usize, *mut *const OrtValue) -> *mut OrtStatus,
    pub KernelContext_GetOutput: unsafe extern "system" fn(*mut OrtKernelContext, usize, *const i64, usize, *mut *mut OrtValue) -> *mut OrtStatus,
    pub ReleaseEnv: unsafe extern "system" fn(*mut OrtEnv) -> (),
    pub ReleaseStatus: unsafe extern "system" fn(*mut OrtStatus) -> (),
    pub ReleaseMemoryInfo: unsafe extern "system" fn(*mut OrtMemoryInfo) -> (),
    pub ReleaseSession: unsafe extern "system" fn(*mut OrtSession) -> (),
    pub ReleaseValue: unsafe extern "system" fn(*mut OrtValue) -> (),
    pub ReleaseRunOptions: unsafe extern "system" fn(*mut OrtRunOptions) -> (),
    pub ReleaseTypeInfo: unsafe extern "system" fn(*mut OrtTypeInfo) -> (),
    pub ReleaseTensorTypeAndShapeInfo: unsafe extern "system" fn(*mut OrtTensorTypeAndShapeInfo) -> (),
    pub ReleaseSessionOptions: unsafe extern "system" fn(*mut OrtSessionOptions) -> (),
    pub ReleaseCustomOpDomain: unsafe extern "system" fn(*mut OrtCustomOpDomain) -> (),
    pub GetDenotationFromTypeInfo: unsafe extern "system" fn(*const OrtTypeInfo, *mut *const c_char, *mut usize) -> *mut OrtStatus,
    pub CastTypeInfoToMapTypeInfo: unsafe extern "system" fn(*const OrtTypeInfo, *mut *const OrtMapTypeInfo) -> *mut OrtStatus,
    pub CastTypeInfoToSequenceTypeInfo: unsafe extern "system" fn(*const OrtTypeInfo, *mut *const OrtSequenceTypeInfo) -> *mut OrtStatus,
    pub GetMapKeyType: unsafe extern "system" fn(*const OrtMapTypeInfo, *mut c_int) -> *mut OrtStatus,
    pub GetMapValueType: unsafe extern "system" fn(*const OrtMapTypeInfo, *mut *mut OrtTypeInfo) -> *mut OrtStatus,
    pub GetSequenceElementType: unsafe extern "system" fn(*const OrtSequenceTypeInfo, *mut *mut OrtTypeInfo) -> *mut OrtStatus,
    pub ReleaseMapTypeInfo: unsafe extern "system" fn(*mut OrtMapTypeInfo) -> (),
    pub ReleaseSequenceTypeInfo: unsafe extern "system" fn(*mut OrtSequenceTypeInfo) -> (),
    pub SessionEndProfiling: unsafe extern "system" fn(*mut OrtSession, *mut OrtAllocator, *mut *mut c_char) -> *mut OrtStatus,
    pub SessionGetModelMetadata: unsafe extern "system" fn(*const OrtSession, *mut *mut OrtModelMetadata) -> *mut OrtStatus,
    pub ModelMetadataGetProducerName: unsafe extern "system" fn(*const OrtModelMetadata, *mut OrtAllocator, *mut *mut c_char) -> *mut OrtStatus,
    pub ModelMetadataGetGraphName: unsafe extern "system" fn(*const OrtModelMetadata, *mut OrtAllocator, *mut *mut c_char) -> *mut OrtStatus,
    pub ModelMetadataGetDomain: unsafe extern "system" fn(*const OrtModelMetadata, *mut OrtAllocator, *mut *mut c_char) -> *mut OrtStatus,
    pub ModelMetadataGetDescription: unsafe extern "system" fn(*const OrtModelMetadata, *mut OrtAllocator, *mut *mut c_char) -> *mut OrtStatus,
    pub ModelMetadataLookupCustomMetadataMap: unsafe extern "system" fn(*const OrtModelMetadata, *mut OrtAllocator, *const c_char, *mut *mut c_char) -> *mut OrtStatus,
    pub ModelMetadataGetVersion: unsafe extern "system" fn(*const OrtModelMetadata, *mut i64) -> *mut OrtStatus,
    pub ReleaseModelMetadata: unsafe extern "system" fn(*mut OrtModelMetadata) -> (),
    pub CreateEnvWithGlobalThreadPools: unsafe extern "system" fn(c_int, *const c_char, *const OrtThreadingOptions, *mut *mut OrtEnv) -> *mut OrtStatus,
    pub DisablePerSessionThreads: unsafe extern "system" fn(*mut OrtSessionOptions) -> *mut OrtStatus,
    pub CreateThreadingOptions: unsafe extern "system" fn(*mut *mut OrtThreadingOptions) -> *mut OrtStatus,
    pub ReleaseThreadingOptions: unsafe extern "system" fn(*mut OrtThreadingOptions) -> (),
    pub ModelMetadataGetCustomMetadataMapKeys: unsafe extern "system" fn(*const OrtModelMetadata, *mut OrtAllocator, *mut *mut *mut c_char, *mut i64) -> *mut OrtStatus,
    pub AddFreeDimensionOverrideByName: unsafe extern "system" fn(*mut OrtSessionOptions, *const c_char, i64) -> *mut OrtStatus,
    pub GetAvailableProviders: unsafe extern "system" fn(*mut *mut *mut c_char, *mut c_int) -> *mut OrtStatus,
    pub ReleaseAvailableProviders: unsafe extern "system" fn(*mut *mut c_char, c_int) -> *mut OrtStatus,
    pub GetStringTensorElementLength: unsafe extern "system" fn(*const OrtValue, usize, *mut usize) -> *mut OrtStatus,
    pub GetStringTensorElement: unsafe extern "system" fn(*const OrtValue, usize, usize, *mut c_void) -> *mut OrtStatus,
    pub FillStringTensorElement: unsafe extern "system" fn(*mut OrtValue, *const c_char, usize) -> *mut OrtStatus,
    pub AddSessionConfigEntry: unsafe extern "system" fn(*mut OrtSessionOptions, *const c_char, *const c_char) -> *mut OrtStatus,
    pub CreateAllocator: unsafe extern "system" fn(*const OrtSession, *const OrtMemoryInfo, *mut *mut OrtAllocator) -> *mut OrtStatus,
    pub ReleaseAllocator: unsafe extern "system" fn(*mut OrtAllocator) -> (),
    pub RunWithBinding: unsafe extern "system" fn(*mut OrtSession, *const OrtRunOptions, *const OrtIoBinding) -> *mut OrtStatus,
    pub CreateIoBinding: unsafe extern "system" fn(*mut OrtSession, *mut *mut OrtIoBinding) -> *mut OrtStatus,
    pub ReleaseIoBinding: unsafe extern "system" fn(*mut OrtIoBinding) -> (),
    pub BindInput: unsafe extern "system" fn(*mut OrtIoBinding, *const c_char, *const OrtValue) -> *mut OrtStatus,
    pub BindOutput: unsafe extern "system" fn(*mut OrtIoBinding, *const c_char, *const OrtValue) -> *mut OrtStatus,
    pub BindOutputToDevice: unsafe extern "system" fn(*mut OrtIoBinding, *const c_char, *const OrtMemoryInfo) -> *mut OrtStatus,
    pub GetBoundOutputNames: unsafe extern "system" fn(*const OrtIoBinding, *mut OrtAllocator, *mut *mut c_char, *mut *mut usize, *mut usize) -> *mut OrtStatus,
    pub GetBoundOutputValues: unsafe extern "system" fn(*const OrtIoBinding, *mut OrtAllocator, *mut *mut *mut OrtValue, *mut usize) -> *mut OrtStatus,
    pub ClearBoundInputs: unsafe extern "system" fn(*mut OrtIoBinding) -> (),
    pub ClearBoundOutputs: unsafe extern "system" fn(*mut OrtIoBinding) -> (),
    pub TensorAt: unsafe extern "system" fn(*mut OrtValue, *const i64, usize, *mut *mut c_void) -> *mut OrtStatus,
    pub CreateAndRegisterAllocator: unsafe extern "system" fn(*mut OrtEnv, *const OrtMemoryInfo, *const OrtArenaCfg) -> *mut OrtStatus,
    pub SetLanguageProjection: unsafe extern "system" fn(*const OrtEnv, c_int) -> *mut OrtStatus,
    pub SessionGetProfilingStartTimeNs: unsafe extern "system" fn(*const OrtSession, *mut u64) -> *mut OrtStatus,
    pub SetGlobalIntraOpNumThreads: unsafe extern "system" fn(*mut OrtThreadingOptions, c_int) -> *mut OrtStatus,
    pub SetGlobalInterOpNumThreads: unsafe extern "system" fn(*mut OrtThreadingOptions, c_int) -> *mut OrtStatus,
    pub SetGlobalSpinControl: unsafe extern "system" fn(*mut OrtThreadingOptions, c_int) -> *mut OrtStatus,
    pub AddInitializer: unsafe extern "system" fn(*mut OrtSessionOptions, *const c_char, *const OrtValue) -> *mut OrtStatus,
    pub CreateEnvWithCustomLoggerAndGlobalThreadPools: unsafe extern "system" fn(OrtCallback, *mut c_void, c_int, *const c_char, *const OrtThreadingOptions, *mut *mut OrtEnv) -> *mut OrtStatus,
    pub SessionOptionsAppendExecutionProvider_CUDA: unsafe extern "system" fn(*mut OrtSessionOptions, *const OrtCUDAProviderOptions) -> *mut OrtStatus,
}

#[repr(C)]
pub struct OrtApiBase {
    pub GetApi: unsafe extern "system" fn(version: u32) -> *const OrtApi,
    pub GetVersionString: unsafe extern "system" fn() -> *const c_char,
}

extern "C" {
    /// Entry point for the matching ONNX Runtime 1.22 library.
    pub fn OrtGetApiBase() -> *const OrtApiBase;
}
