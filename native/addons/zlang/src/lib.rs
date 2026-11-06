//! zlang — Native language detection for Linux using whatlang
//!
//! Clean-room implementation providing BCP-47 hypothesis tags and probability scores
//! compatible with Zalo's zlang NAPI interface.

use napi::bindgen_prelude::AsyncTask;
use napi::{Env, Result, Task};
use napi_derive::napi;

mod backend;

#[napi(object)]
pub struct Hypothesis {
    pub tag: String,
    pub confidence: Option<f64>,
}

pub struct DetectTask {
    text: String,
    max_results: Option<u32>,
}

impl Task for DetectTask {
    type Output = Vec<backend::Hypothesis>;
    type JsValue = Vec<Hypothesis>;

    fn compute(&mut self) -> Result<Self::Output> {
        backend::detect(&self.text, self.max_results)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output
            .into_iter()
            .map(|(tag, confidence)| Hypothesis { tag, confidence })
            .collect())
    }
}

#[napi]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[napi]
pub fn backend() -> String {
    backend::name().to_string()
}

#[napi]
pub fn available() -> bool {
    backend::available()
}

#[napi]
pub fn scores() -> String {
    backend::score_kind().to_string()
}

#[napi]
pub fn detect(text: String, max_results: Option<u32>) -> AsyncTask<DetectTask> {
    AsyncTask::new(DetectTask { text, max_results })
}
