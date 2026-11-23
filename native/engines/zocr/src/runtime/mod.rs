//! ONNX Runtime is dlopen'ed from `libonnxruntime.so` beside the binary via
//! [`ort::init_from`]; there is still no link-time dependency on the runtime.
mod segments;

use crate::config::Config;
use anyhow::{bail, ensure, Context, Result};
use ort::{
    environment::Environment,
    logging::LogLevel,
    session::{
        builder::{GraphOptimizationLevel, SessionBuilder},
        NoSelectedOutputs, RunOptions, Session as OrtSession,
    },
    value::{Outlet, Shape, Tensor as OrtTensor, TensorElementType, TensorRef, ValueType},
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

// `ort` allows exactly one environment per process; keep it alive for the whole
// process and hand out clones, so repeated `Runtime::load` calls stay cheap.
static ENV: OnceLock<Result<Environment, String>> = OnceLock::new();

fn environment(path: &Path) -> Result<Environment> {
    ENV.get_or_init(|| {
        let env = ort::init_from(path)
            .map_err(|error| error.to_string())?
            .with_name("zocr-host")
            .build()
            .map_err(|error| error.to_string())?;
        env.set_log_level(LogLevel::Error);
        Ok(env)
    })
    .clone()
    .map_err(|error| anyhow::anyhow!("unable to create ONNX Runtime environment: {error}"))
}

fn step(result: Result<SessionBuilder, ort::Error<SessionBuilder>>) -> Result<SessionBuilder> {
    result.map_err(|error| anyhow::anyhow!("{}", error.message()))
}

fn input_dims(outlet: &Outlet, rank: usize, ty: TensorElementType) -> Result<Vec<i64>> {
    let ValueType::Tensor {
        ty: actual, shape, ..
    } = outlet.dtype()
    else {
        bail!("model input is not a tensor");
    };
    ensure!(*actual == ty, "unexpected model input dtype");
    ensure!(shape.len() == rank, "unexpected model input rank");
    Ok(shape.iter().copied().collect())
}

pub struct Runtime {
    env: Environment,
    active_runs: Mutex<Vec<Arc<RunOptions<NoSelectedOutputs>>>>,
    cancelled: AtomicBool,
}

impl Runtime {
    pub fn load(dir: &Path) -> Result<Arc<Self>> {
        let path = dir.join("libonnxruntime.so");
        ensure!(
            path.is_file(),
            "libonnxruntime.so not found alongside binary"
        );
        let env = environment(&path)?;
        Ok(Arc::new(Self {
            env,
            active_runs: Mutex::new(Vec::new()),
            cancelled: AtomicBool::new(false),
        }))
    }

    pub fn cancel_active(&self) {
        if let Ok(runs) = self.active_runs.lock() {
            self.cancelled.store(true, Ordering::Relaxed);
            for options in runs.iter() {
                let _ = options.terminate();
            }
        }
    }

    pub fn session(self: &Arc<Self>, model: &[u8], cfg: &Config, label: &str) -> Result<Session> {
        let mut builder =
            OrtSession::builder(&self.env).context("unable to create session builder")?;
        builder = step(builder.with_intra_threads(cfg.parallel_budget))?;
        builder = step(builder.with_inter_threads(1))?;
        builder = step(builder.with_parallel_execution(false))?;
        builder = step(builder.with_optimization_level(GraphOptimizationLevel::Level3))?;
        builder = step(builder.with_memory_pattern(true))?;
        builder = step(builder.with_log_level(LogLevel::Error))?;
        if let Some(dir) = &cfg.profile_dir {
            std::fs::create_dir_all(dir).context("unable to create ORT profile directory")?;
            builder = step(builder.with_profiling(dir.join(format!("ort-session-{label}"))))?;
        }
        let session = builder
            .commit_from_memory(model)
            .with_context(|| format!("failed to initialize {label}"))?;
        let inputs = session.inputs();
        ensure!(
            (inputs.len() == 1 || (label == "recognizer" && inputs.len() == 2))
                && !session.outputs().is_empty(),
            "{label}: unsupported model input/output count"
        );
        let input_shape = input_dims(&inputs[0], 4, TensorElementType::Float32)?;
        ensure!(
            input_shape
                .iter()
                .all(|&d| d == -1 || (1..=4096).contains(&d)),
            "unsupported model input dimension"
        );
        ensure!(
            input_shape[1] == -1 || input_shape[1] == 3,
            "OCR models require three input channels"
        );
        let segment_input = if inputs.len() == 2 {
            let name = inputs[1].name().to_string();
            ensure!(name == "seg", "recognizer auxiliary input must be seg");
            Some((name, input_dims(&inputs[1], 2, TensorElementType::Int32)?))
        } else {
            None
        };
        Ok(Session {
            inner: session,
            runtime: self.clone(),
            input_shape,
            segment_input,
            profiling: cfg.profile_dir.is_some(),
        })
    }
}

struct RunGuard {
    runtime: Arc<Runtime>,
    options: Arc<RunOptions<NoSelectedOutputs>>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.runtime.active_runs.lock() {
            runs.retain(|entry| !Arc::ptr_eq(entry, &self.options));
        }
    }
}

pub struct Session {
    inner: OrtSession,
    runtime: Arc<Runtime>,
    input_shape: Vec<i64>,
    segment_input: Option<(String, Vec<i64>)>,
    profiling: bool,
}

impl Session {
    pub fn input_shape(&self) -> &[i64] {
        &self.input_shape
    }

    pub fn has_segments(&self) -> bool {
        self.segment_input.is_some()
    }

    pub fn run(&mut self, shape: &[i64], data: &[f32]) -> Result<Tensor> {
        self.run_with_widths(shape, data, None)
    }

    pub fn run_recognition(
        &mut self,
        shape: &[i64],
        data: &[f32],
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
        data: &[f32],
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
        let image = TensorRef::from_array_view((Shape::new(shape.iter().copied()), data))
            .context("unable to create input tensor")?;
        let options = Arc::new(RunOptions::new().context("unable to create run options")?);
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
            runs.push(options.clone());
        }
        let _guard = RunGuard {
            runtime: self.runtime.clone(),
            options: options.clone(),
        };
        let seg = if let Some((_, expected)) = &self.segment_input {
            let full_widths = vec![shape[3] as u32; shape[0] as usize];
            let (segment_shape, values) = segments::build(shape, widths.unwrap_or(&full_widths))?;
            ensure!(
                segment_shape
                    .iter()
                    .zip(expected)
                    .all(|(&actual, &expected)| expected < 0 || actual == expected),
                "seg shape does not match model"
            );
            Some(
                OrtTensor::from_array((Shape::new(segment_shape), values))
                    .context("unable to create seg tensor")?,
            )
        } else {
            None
        };
        let outputs = if let Some(seg) = seg {
            self.inner
                .run_with_options([image.into(), seg.into()], &options)?
        } else {
            self.inner.run_with_options([image.into()], &options)?
        };
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .context("expected float32 output")?;
        ensure!(shape.len() <= 8, "output tensor rank exceeds limit");
        ensure!(
            !data.is_empty() && data.len() <= 134_217_728,
            "output tensor size exceeds limit or is empty"
        );
        ensure!(
            data.iter().all(|n| n.is_finite()),
            "non-finite model output"
        );
        Ok(Tensor {
            shape: shape.to_vec(),
            data: data.to_vec(),
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.profiling {
            let _ = self.inner.end_profiling();
        }
    }
}

pub struct Tensor {
    pub shape: Vec<i64>,
    pub data: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, vault::Vault};
    use std::time::Duration;

    fn test_config() -> Config {
        Config {
            auth_token: String::new(),
            skip_cpu_gate: true,
            pipe_buffer: 1 << 20,
            frame_timeout: Duration::from_millis(15),
            parallel_budget: 2,
            max_pixel_size: 1280,
            orientation: false,
            orientation_threshold: 0.9,
            orientation_probe: 5,
            score_filter: true,
            det_min: 0.741,
            rec_min: 0.84,
            rec_keep: 0.925,
            session_ttl: None,
            affinity: "auto".into(),
            profile_dir: None,
            benchmark_resources: false,
            benchmark_profile: false,
        }
    }

    #[test]
    fn runs_bundled_recognizer() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../app/native/ocr");
        if !dir.join("zocr/models.zmdl").is_file() {
            eprintln!("skipping: bundled OCR assets absent");
            return;
        }
        let runtime = Runtime::load(&dir).expect("loads bundled libonnxruntime.so");
        let info = ort::info();
        eprintln!("ONNX Runtime build: {info}");
        assert!(!info.trim().is_empty(), "empty ONNX Runtime build info");
        let vault = Vault::load(&dir).expect("vault");
        let model = vault.decrypt(1).expect("recognizer model");
        let mut session = runtime
            .session(&model, &test_config(), "recognizer")
            .expect("recognizer session");
        let dims = session.input_shape().to_vec();
        let batch = if dims[0] > 0 { dims[0] } else { 1 };
        let width = if dims[3] > 0 { dims[3] } else { 256 };
        let shape = [batch, 3, 48, width];
        let len = usize::try_from(batch * 3 * 48 * width).expect("tensor size fits usize");
        let data = vec![0f32; len];
        let widths = vec![
            u32::try_from(width).expect("width fits u32");
            usize::try_from(batch).expect("batch fits usize")
        ];
        let out = session
            .run_recognition(&shape, &data, &widths)
            .expect("inference");
        assert!(!out.data.is_empty() && out.data.iter().all(|v| v.is_finite()));
    }
}
