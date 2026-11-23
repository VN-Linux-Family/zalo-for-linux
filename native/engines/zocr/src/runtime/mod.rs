//! Dynamically loaded ONNX C API; no link-time dependency on the runtime .so.
mod segments;
use crate::config::Config;
use anyhow::{bail, ensure, Context, Result};
use libloading::Library;
use ort_sys as ffi;
use std::{
    ffi::{CStr, CString},
    os::unix::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

pub struct Runtime {
    api: *const ffi::OrtApi,
    env: *mut ffi::OrtEnv,
    _library: Library,
    active_runs: Mutex<Vec<usize>>,
    cancelled: AtomicBool,
}
// ORT Env and sessions support concurrent callers. Session::run requires &mut self.
// SAFETY: `Runtime` owns the ORT env/api pointers and keeps their library loaded; ORT env calls are safe from any thread.
unsafe impl Send for Runtime {}
// SAFETY: shared mutable state is behind `Mutex`/`AtomicBool`, and ORT env calls are thread-safe.
unsafe impl Sync for Runtime {}
impl Runtime {
    pub fn load(dir: &Path) -> Result<Arc<Self>> {
        let path = dir.join("libonnxruntime.so");
        ensure!(
            path.is_file(),
            "libonnxruntime.so not found alongside binary"
        );
        // SAFETY: `path` was checked as a regular file; loading the trusted runtime beside the binary only runs its constructors, and every returned pointer is null-checked before use.
        unsafe {
            let library =
                Library::new(&path).context("unable to load libonnxruntime.so alongside binary")?;
            let get_base = library
                .get::<unsafe extern "system" fn() -> *const ffi::OrtApiBase>(b"OrtGetApiBase\0")
                .context("runtime does not export OrtGetApiBase")?;
            let base = get_base();
            ensure!(!base.is_null(), "null OrtApiBase");
            let api = ((*base).GetApi)(ffi::ORT_API_VERSION);
            ensure!(
                !api.is_null(),
                "runtime does not support ONNX C API {}",
                ffi::ORT_API_VERSION
            );
            let mut runtime = Self {
                api,
                env: ptr::null_mut(),
                _library: library,
                active_runs: Mutex::new(Vec::new()),
                cancelled: AtomicBool::new(false),
            };
            let status = ((*api).CreateEnv)(
                ffi::OrtLoggingLevel::ORT_LOGGING_LEVEL_ERROR,
                c"zocr-host".as_ptr(),
                &raw mut runtime.env,
            );
            runtime.check(status)?;
            Ok(Arc::new(runtime))
        }
    }
    fn check(&self, status: ffi::OrtStatusPtr) -> Result<()> {
        let status = status.0;
        if status.is_null() {
            return Ok(());
        }
        // SAFETY: `status` is non-null (checked above) and owned by ORT; its message stays valid until the `ReleaseStatus` call below.
        unsafe {
            let message = CStr::from_ptr(((*self.api).GetErrorMessage)(status))
                .to_string_lossy()
                .into_owned();
            ((*self.api).ReleaseStatus)(status);
            bail!("ONNX Runtime: {message}")
        }
    }
    pub fn cancel_active(&self) {
        if let Ok(runs) = self.active_runs.lock() {
            self.cancelled.store(true, Ordering::Relaxed);
            for &options in runs.iter() {
                // SAFETY: entries are registered only after `CreateRunOptions` succeeds and are removed under this lock before release, so the pointer is live.
                unsafe {
                    let _ = self.check(((*self.api).RunOptionsSetTerminate)(
                        options as *mut ffi::OrtRunOptions,
                    ));
                }
            }
        }
    }
    pub fn session(self: &Arc<Self>, model: &[u8], cfg: &Config, label: &str) -> Result<Session> {
        // SAFETY: `self.api` is the live ORT table of the loaded library; every out-pointer is validated by `check` and released by its owning wrapper, and `model` lives through the call.
        unsafe {
            let api = &*self.api;
            let mut options = ptr::null_mut();
            self.check((api.CreateSessionOptions)(&raw mut options))?;
            let options = SessionOptions {
                runtime: self.clone(),
                ptr: options,
            };
            self.check((api.SetIntraOpNumThreads)(
                options.ptr,
                i32::try_from(cfg.parallel_budget)
                    .expect("parallel budget is clamped to the CPU count"),
            ))?;
            self.check((api.SetInterOpNumThreads)(options.ptr, 1))?;
            self.check((api.SetSessionExecutionMode)(
                options.ptr,
                ffi::ExecutionMode::ORT_SEQUENTIAL,
            ))?;
            self.check((api.SetSessionGraphOptimizationLevel)(
                options.ptr,
                ffi::GraphOptimizationLevel::ORT_ENABLE_ALL,
            ))?;
            self.check((api.EnableCpuMemArena)(options.ptr))?;
            self.check((api.EnableMemPattern)(options.ptr))?;
            self.check((api.SetSessionLogSeverityLevel)(options.ptr, 3))?;
            if let Some(dir) = &cfg.profile_dir {
                std::fs::create_dir_all(dir).context("unable to create ORT profile directory")?;
                let prefix = CString::new(
                    dir.join(format!("ort-session-{label}"))
                        .as_os_str()
                        .as_bytes(),
                )?;
                self.check((api.EnableProfiling)(options.ptr, prefix.as_ptr()))?;
            }
            let mut session = Session {
                ptr: ptr::null_mut(),
                runtime: self.clone(),
                input: CString::new("")?,
                output: CString::new("")?,
                input_shape: Vec::new(),
                segment_input: None,
                profiling: cfg.profile_dir.is_some(),
            };
            self.check((api.CreateSessionFromArray)(
                self.env,
                model.as_ptr().cast(),
                model.len(),
                options.ptr,
                &raw mut session.ptr,
            ))
            .with_context(|| format!("failed to initialize {label}"))?;
            let mut input_count = 0;
            let mut output_count = 0;
            self.check((api.SessionGetInputCount)(
                session.ptr,
                &raw mut input_count,
            ))?;
            self.check((api.SessionGetOutputCount)(
                session.ptr,
                &raw mut output_count,
            ))?;
            ensure!(
                (input_count == 1 || (label == "recognizer" && input_count == 2))
                    && output_count >= 1,
                "{label}: unsupported model input/output count"
            );
            session.input = session.name(true, 0)?;
            session.output = session.name(false, 0)?;
            session.input_shape = session.read_input_shape(
                0,
                4,
                ffi::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
            )?;
            if input_count == 2 {
                let name = session.name(true, 1)?;
                ensure!(
                    name.as_bytes() == b"seg",
                    "recognizer auxiliary input must be seg"
                );
                let shape = session.read_input_shape(
                    1,
                    2,
                    ffi::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_INT32,
                )?;
                session.segment_input = Some((name, shape));
            }
            Ok(session)
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if !self.env.is_null() {
            // SAFETY: `env` is non-null and was produced by `CreateEnv`; `Drop` runs once, so it is released exactly once.
            unsafe {
                ((*self.api).ReleaseEnv)(self.env);
            }
        }
    }
}
struct SessionOptions {
    runtime: Arc<Runtime>,
    ptr: *mut ffi::OrtSessionOptions,
}
impl Drop for SessionOptions {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from a successful `CreateSessionOptions` (errors discard the wrapper), and this is its only release.
        unsafe {
            ((*self.runtime.api).ReleaseSessionOptions)(self.ptr);
        }
    }
}

pub struct Session {
    ptr: *mut ffi::OrtSession,
    runtime: Arc<Runtime>,
    input: CString,
    output: CString,
    input_shape: Vec<i64>,
    segment_input: Option<(CString, Vec<i64>)>,
    profiling: bool,
}
// SAFETY: an ORT session may move between threads; concurrent runs are impossible because `run`/`run_recognition` need `&mut self`.
unsafe impl Send for Session {}
impl Session {
    fn name(&self, input: bool, index: usize) -> Result<CString> {
        // SAFETY: `Session` keeps the runtime loaded and `self.ptr` a live ORT session.
        unsafe {
            let api = &*self.runtime.api;
            let mut allocator = ptr::null_mut();
            self.runtime
                .check((api.GetAllocatorWithDefaultOptions)(&raw mut allocator))?;
            let mut name = ptr::null_mut();
            if input {
                self.runtime.check((api.SessionGetInputName)(
                    self.ptr,
                    index,
                    allocator,
                    &raw mut name,
                ))?;
            } else {
                self.runtime.check((api.SessionGetOutputName)(
                    self.ptr,
                    index,
                    allocator,
                    &raw mut name,
                ))?;
            }
            ensure!(!name.is_null(), "runtime returned a null tensor name");
            let copy = CStr::from_ptr(name).to_owned();
            self.runtime
                .check((api.AllocatorFree)(allocator, name.cast()))?;
            Ok(copy)
        }
    }
    fn read_input_shape(
        &self,
        index: usize,
        expected_rank: usize,
        expected_type: ffi::ONNXTensorElementDataType,
    ) -> Result<Vec<i64>> {
        // SAFETY: `Session` keeps the runtime loaded and `self.ptr` a live ORT session.
        unsafe {
            let api = &*self.runtime.api;
            let mut info = TypeInfo {
                runtime: self.runtime.clone(),
                ptr: ptr::null_mut(),
            };
            self.runtime.check((api.SessionGetInputTypeInfo)(
                self.ptr,
                index,
                &raw mut info.ptr,
            ))?;
            let mut tensor = ptr::null();
            self.runtime
                .check((api.CastTypeInfoToTensorInfo)(info.ptr, &raw mut tensor))?;
            ensure!(!tensor.is_null(), "model input is not a tensor");
            let mut rank = 0;
            self.runtime
                .check((api.GetDimensionsCount)(tensor, &raw mut rank))?;
            ensure!(rank == expected_rank, "unexpected model input rank");
            let mut dtype = ffi::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_UNDEFINED;
            self.runtime
                .check((api.GetTensorElementType)(tensor, &raw mut dtype))?;
            ensure!(dtype == expected_type, "unexpected model input dtype");
            let mut shape = vec![0; rank];
            self.runtime
                .check((api.GetDimensions)(tensor, shape.as_mut_ptr(), rank))?;
            ensure!(
                shape.iter().all(|&d| d == -1 || (1..=4096).contains(&d)),
                "unsupported model input dimension"
            );
            ensure!(
                rank != 4 || shape[1] == -1 || shape[1] == 3,
                "OCR models require three input channels"
            );
            Ok(shape)
        }
    }
    pub fn input_shape(&self) -> &[i64] {
        &self.input_shape
    }
    pub fn has_segments(&self) -> bool {
        self.segment_input.is_some()
    }
    pub fn run(&mut self, shape: &[i64], data: &mut [f32]) -> Result<Tensor> {
        self.run_with_widths(shape, data, None)
    }
    pub fn run_recognition(
        &mut self,
        shape: &[i64],
        data: &mut [f32],
        widths: &[u32],
    ) -> Result<Tensor> {
        self.run_with_widths(shape, data, Some(widths))
    }
    #[allow(
        clippy::too_many_lines,
        reason = "single protocol pipeline, splitting hurts readability"
    )]
    fn run_with_widths(
        &mut self,
        shape: &[i64],
        data: &mut [f32],
        widths: Option<&[u32]>,
    ) -> Result<Tensor> {
        let count = shape
            .iter()
            .try_fold(1usize, |n, &d| n.checked_mul(usize::try_from(d).ok()?))
            .context("invalid input shape")?;
        ensure!(count == data.len(), "input shape/data length mismatch");
        ensure!(
            shape.len() == self.input_shape.len()
                && shape
                    .iter()
                    .zip(&self.input_shape)
                    .all(|(&actual, &expected)| expected < 0 || actual == expected),
            "input shape {:?} does not match model {:?}",
            shape,
            self.input_shape
        );
        // SAFETY: ORT objects come from the live runtime; `data`/`segment_data` outlive every call, all out-pointers are checked and released by their wrappers, and the output buffer is copied with ORT's reported length.
        unsafe {
            let api = &*self.runtime.api;
            let mut memory = MemoryInfo {
                runtime: self.runtime.clone(),
                ptr: ptr::null_mut(),
            };
            self.runtime.check((api.CreateCpuMemoryInfo)(
                ffi::OrtAllocatorType::OrtArenaAllocator,
                ffi::OrtMemType::OrtMemTypeDefault,
                &raw mut memory.ptr,
            ))?;
            let mut input = Value {
                runtime: self.runtime.clone(),
                ptr: ptr::null_mut(),
            };
            self.runtime.check((api.CreateTensorWithDataAsOrtValue)(
                memory.ptr,
                data.as_mut_ptr().cast(),
                std::mem::size_of_val(data),
                shape.as_ptr(),
                shape.len(),
                ffi::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
                &raw mut input.ptr,
            ))?;
            let input_name = self.input.as_ptr();
            let output_name = self.output.as_ptr();
            let input_ptr: *const ffi::OrtValue = input.ptr;
            let mut output = Value {
                runtime: self.runtime.clone(),
                ptr: ptr::null_mut(),
            };
            let mut options = RunOptions {
                runtime: self.runtime.clone(),
                ptr: ptr::null_mut(),
            };
            self.runtime
                .check((api.CreateRunOptions)(&raw mut options.ptr))?;
            {
                let mut runs = self
                    .runtime
                    .active_runs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("run registry poisoned"))?;
                ensure!(
                    !self.runtime.cancelled.load(Ordering::Relaxed),
                    "inference cancelled by shutdown"
                );
                runs.push(options.ptr as usize);
            }
            let mut segment_data: Vec<i32>;
            let mut segment_value = Value {
                runtime: self.runtime.clone(),
                ptr: ptr::null_mut(),
            };
            if let Some((name, expected)) = &self.segment_input {
                let full_widths = vec![shape[3] as u32; shape[0] as usize];
                let (segment_shape, values) =
                    segments::build(shape, widths.unwrap_or(&full_widths))?;
                ensure!(
                    segment_shape
                        .iter()
                        .zip(expected)
                        .all(|(&actual, &expected)| expected < 0 || actual == expected),
                    "seg shape does not match model"
                );
                segment_data = values;
                self.runtime.check((api.CreateTensorWithDataAsOrtValue)(
                    memory.ptr,
                    segment_data.as_mut_ptr().cast(),
                    std::mem::size_of_val(segment_data.as_slice()),
                    segment_shape.as_ptr(),
                    2,
                    ffi::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_INT32,
                    &raw mut segment_value.ptr,
                ))?;
                let names = [input_name, name.as_ptr()];
                let inputs = [input_ptr, segment_value.ptr.cast_const()];
                self.runtime.check((api.Run)(
                    self.ptr,
                    options.ptr,
                    names.as_ptr(),
                    inputs.as_ptr(),
                    2,
                    &raw const output_name,
                    1,
                    &raw mut output.ptr,
                ))?;
            } else {
                self.runtime.check((api.Run)(
                    self.ptr,
                    options.ptr,
                    &raw const input_name,
                    &raw const input_ptr,
                    1,
                    &raw const output_name,
                    1,
                    &raw mut output.ptr,
                ))?;
            }
            let mut info = TensorInfo {
                runtime: self.runtime.clone(),
                ptr: ptr::null_mut(),
            };
            self.runtime
                .check((api.GetTensorTypeAndShape)(output.ptr, &raw mut info.ptr))?;
            let mut dtype = ffi::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_UNDEFINED;
            self.runtime
                .check((api.GetTensorElementType)(info.ptr, &raw mut dtype))?;
            ensure!(
                dtype == ffi::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
                "expected float32 output"
            );
            let mut rank = 0;
            let mut len = 0;
            self.runtime
                .check((api.GetDimensionsCount)(info.ptr, &raw mut rank))?;
            ensure!(rank <= 8, "output tensor rank exceeds limit");
            let mut shape = vec![0i64; rank];
            self.runtime
                .check((api.GetDimensions)(info.ptr, shape.as_mut_ptr(), rank))?;
            self.runtime
                .check((api.GetTensorShapeElementCount)(info.ptr, &raw mut len))?;
            ensure!(
                len <= 134_217_728 && len > 0,
                "output tensor size exceeds limit or is empty"
            );
            let mut buffer = ptr::null_mut();
            self.runtime
                .check((api.GetTensorMutableData)(output.ptr, &raw mut buffer))?;
            ensure!(!buffer.is_null(), "null output tensor data");
            let data = std::slice::from_raw_parts(buffer.cast::<f32>(), len).to_vec();
            ensure!(
                data.iter().all(|n| n.is_finite()),
                "non-finite model output"
            );
            Ok(Tensor { shape, data })
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }
        // SAFETY: `self.ptr` is non-null here and came from a successful `CreateSessionFromArray`; this drop releases it once and frees any profiling filename first.
        unsafe {
            let api = &*self.runtime.api;
            if self.profiling {
                let mut allocator = ptr::null_mut();
                if self
                    .runtime
                    .check((api.GetAllocatorWithDefaultOptions)(&raw mut allocator))
                    .is_ok()
                {
                    let mut filename = ptr::null_mut();
                    if self
                        .runtime
                        .check((api.SessionEndProfiling)(
                            self.ptr,
                            allocator,
                            &raw mut filename,
                        ))
                        .is_ok()
                        && !filename.is_null()
                    {
                        let _ = self
                            .runtime
                            .check((api.AllocatorFree)(allocator, filename.cast()));
                    }
                }
            }
            (api.ReleaseSession)(self.ptr);
        }
    }
}
struct RunOptions {
    runtime: Arc<Runtime>,
    ptr: *mut ffi::OrtRunOptions,
}
impl Drop for RunOptions {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }
        if let Ok(mut runs) = self.runtime.active_runs.lock() {
            runs.retain(|&p| p != self.ptr as usize);
        }
        // SAFETY: `ptr` is non-null from a successful `CreateRunOptions`; it was just unregistered under the registry lock, so this is its only release.
        unsafe {
            ((*self.runtime.api).ReleaseRunOptions)(self.ptr);
        }
    }
}
pub struct Tensor {
    pub shape: Vec<i64>,
    pub data: Vec<f32>,
}
macro_rules! resource {
    ($name:ident, $ty:ty, $release:ident) => {
        struct $name {
            runtime: Arc<Runtime>,
            ptr: *mut $ty,
        }
        impl Drop for $name {
            fn drop(&mut self) {
                if !self.ptr.is_null() {
                    // SAFETY: the pointer was produced by the matching ORT constructor, the null guard covers unused slots, and `Drop` releases it exactly once.
                    unsafe {
                        ((*self.runtime.api).$release)(self.ptr);
                    }
                }
            }
        }
    };
}
resource!(MemoryInfo, ffi::OrtMemoryInfo, ReleaseMemoryInfo);
resource!(Value, ffi::OrtValue, ReleaseValue);
resource!(
    TensorInfo,
    ffi::OrtTensorTypeAndShapeInfo,
    ReleaseTensorTypeAndShapeInfo
);

resource!(TypeInfo, ffi::OrtTypeInfo, ReleaseTypeInfo);
