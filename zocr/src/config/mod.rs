use anyhow::{bail, Context, Result};
use std::{env, path::PathBuf, time::Duration};

// The loaded models hold roughly 450 MB and OCR is used in short bursts, so
// drop them after a short idle period; the next request reloads them.
const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct Config {
    pub auth_token: String,
    pub skip_cpu_gate: bool,
    pub pipe_buffer: usize,
    pub frame_timeout: Duration,
    pub parallel_budget: usize,
    pub max_pixel_size: u32,
    pub orientation: bool,
    pub orientation_threshold: f32,
    pub orientation_probe: usize,
    pub score_filter: bool,
    pub det_min: f32,
    pub rec_min: f32,
    pub rec_keep: f32,
    pub trim_interval: Option<Duration>,
    pub session_ttl: Option<Duration>,
    pub affinity: String,
    pub profile_dir: Option<PathBuf>,
    pub benchmark_resources: bool,
    pub benchmark_profile: bool,
}
impl Config {
    pub fn from_env(physical_cores: usize) -> Result<Self> {
        let auth_token =
            env::var("ZOCR_AUTH_TOKEN").context("missing or empty ZOCR_AUTH_TOKEN")?;
        if auth_token.is_empty() || auth_token.len() > 65536 {
            bail!("invalid ZOCR_AUTH_TOKEN length");
        }
        let cfg =
            Self {
                auth_token,
                skip_cpu_gate: flag("ZOCR_SKIP_CPU_GATE", false)?,
                pipe_buffer: positive("ZOCR_PIPE_BUFFER_KIB", 1024, 65536)? *
                    1024,
                frame_timeout: Duration::from_millis(positive("ZOCR_PIPE_FRAME_TIMEOUT_MS",
                                15, 600_000)? as u64),
                parallel_budget: positive("ZOCR_MAX_PARALLEL_TASKS",
                            physical_cores, 1024)?.min(physical_cores),
                max_pixel_size: positive("ZOCR_MAX_PIXEL_SIZE", 1280, 4096)?
                    as u32,
                orientation: flag("ZOCR_TEXTLINE_ORIENTATION", false)?,
                orientation_threshold: probability("ZOCR_TEXTLINE_ORIENTATION_THRESHOLD",
                        0.900)?,
                orientation_probe: positive("ZOCR_TEXTLINE_ORIENTATION_PROBE",
                        5, 1000)?,
                score_filter: flag("ZOCR_LINE_SCORE_FILTER", true)?,
                det_min: probability("ZOCR_LINE_SCORE_DET_MIN", 0.741)?,
                rec_min: probability("ZOCR_LINE_SCORE_REC_MIN", 0.840)?,
                rec_keep: probability("ZOCR_LINE_SCORE_REC_KEEP", 0.925)?,
                trim_interval: duration("ZOCR_WORKING_SET_TRIM_MS", None)?,
                session_ttl: duration("ZOCR_SESSION_IDLE_TTL_MS",
                        Some(DEFAULT_SESSION_TTL))?,
                affinity: env::var("ZOCR_ORT_TOPOLOGY_AFFINITY").unwrap_or_else(|_|
                        "auto".into()),
                profile_dir: env::var_os("ZOCR_ORT_PROFILE_DIR").filter(|x|
                            !x.is_empty()).map(PathBuf::from),
                benchmark_resources: flag("ZOCR_BENCHMARK_RESOURCES", false)?,
                benchmark_profile: flag("ZOCR_BENCHMARK_PROFILE", false)?,
            };
        if cfg.rec_keep < cfg.rec_min {
            bail!("ZOCR_LINE_SCORE_REC_KEEP must be >= ZOCR_LINE_SCORE_REC_MIN");
        }
        Ok(cfg)
    }
}
fn flag(name: &str, default: bool) -> Result<bool> {
    match env::var(name).as_deref() {
        Ok("0" | "off") => Ok(false),
        Ok("1" | "on") => Ok(true),
        Err(env::VarError::NotPresent) => Ok(default),
        _ => bail!("{name} must be 0, 1, off or on"),
    }
}
fn positive(name: &str, default: usize, max: usize) -> Result<usize> {
    let n =
        match env::var(name) {
            Ok(v) =>
                v.parse::<usize>().with_context(||
                            format!("invalid {name}"))?,
            Err(env::VarError::NotPresent) => default,
            Err(e) => return Err(e.into()),
        };
    if n == 0 || n > max { bail!("{name} must be in 1..={max}"); }
    Ok(n)
}
fn probability(name: &str, default: f32) -> Result<f32> {
    let n =
        match env::var(name) {
            Ok(v) =>
                v.parse::<f32>().with_context(|| format!("invalid {name}"))?,
            Err(env::VarError::NotPresent) => default,
            Err(e) => return Err(e.into()),
        };
    if !n.is_finite() || !(0.0..=1.0).contains(&n) {
        bail!("{name} must be finite and in 0..=1");
    }
    Ok(n)
}
fn duration(name: &str, default: Option<Duration>)
    -> Result<Option<Duration>> {
    match env::var(name) {
        Ok(v) => parse_duration(name, Some(&v), default),
        Err(env::VarError::NotPresent) => parse_duration(name, None, default),
        Err(e) => Err(anyhow::anyhow!("{name}: {e}")),
    }
}
// Zalo fills these from its remote config and sends 0 when the key is
// missing, so 0 means "no preference" rather than "never". Use "off" to
// disable.
fn parse_duration(name: &str, raw: Option<&str>, default: Option<Duration>)
    -> Result<Option<Duration>> {
    match raw {
        Some("off") => Ok(None),
        None => Ok(default),
        Some(v) => {
            let n =
                v.parse::<u64>().with_context(|| format!("invalid {name}"))?;
            if n == 0 { return Ok(default); }
            Ok(Some(Duration::from_millis(n)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: Option<Duration> = Some(Duration::from_secs(120));

    #[test]
    fn zero_uses_default() {
        // Zalo's remote config sends 0 when it has no value.
        assert_eq!(parse_duration("X", Some("0"), DEFAULT).unwrap(), DEFAULT);
    }

    #[test]
    fn unset_uses_default() {
        assert_eq!(parse_duration("X", None, DEFAULT).unwrap(), DEFAULT);
    }

    #[test]
    fn off_disables() {
        assert_eq!(parse_duration("X", Some("off"), DEFAULT).unwrap(), None);
    }

    #[test]
    fn explicit_value() {
        assert_eq!(parse_duration("X", Some("1500"), DEFAULT).unwrap(),
            Some(Duration::from_millis(1500)));
        assert!(parse_duration("X", Some("soon"), DEFAULT).is_err());
    }
}
