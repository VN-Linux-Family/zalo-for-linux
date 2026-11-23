use crate::{config::Config, engine::Engine};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Cpu {
    pub physical: usize,
    pub logical: usize,
    pub representatives: Vec<usize>,
    pub allowed: Vec<usize>,
}
impl Cpu {
    pub fn discover() -> Result<Self> {
        // SAFETY: `cpu_set_t` is plain-old-data, so an all-zero bit pattern is a valid empty set.
        let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        ensure!(
            // SAFETY: `mask` is the initialized stack affinity set and `size_of_val` passes exactly its size.
            unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&mask), &raw mut mask) } == 0,
            "sched_getaffinity: {}",
            std::io::Error::last_os_error()
        );
        let allowed: Vec<_> = (0..libc::CPU_SETSIZE as usize)
            // SAFETY: `i` is bounded by `CPU_SETSIZE` and `mask` is the initialized set above.
            .filter(|&i| unsafe { libc::CPU_ISSET(i, &mask) })
            .collect();
        ensure!(!allowed.is_empty(), "empty CPU affinity mask");
        let mut cores = BTreeMap::new();
        for &cpu in &allowed {
            let prefix = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            let package = fs::read_to_string(format!("{prefix}/physical_package_id"))
                .unwrap_or_else(|_| "0".into());
            let core =
                fs::read_to_string(format!("{prefix}/core_id")).unwrap_or_else(|_| cpu.to_string());
            cores
                .entry((package.trim().to_string(), core.trim().to_string()))
                .or_insert(cpu);
        }
        Ok(Self {
            physical: cores.len(),
            logical: allowed.len(),
            representatives: cores.into_values().collect(),
            allowed,
        })
    }
    pub fn gate(skip: bool) -> Result<()> {
        #[cfg(target_arch = "x86_64")]
        if !std::is_x86_feature_detected!("avx2") || !std::is_x86_feature_detected!("fma") {
            if !skip {
                anyhow::bail!("CPU requires AVX2 and FMA; set ZOCR_SKIP_CPU_GATE=1 to override");
            }
            eprintln!("[zocr-host] WARNING: missing AVX2/FMA, performance may degrade");
        }
        #[cfg(target_arch = "aarch64")]
        {
            let _ = skip;
            ensure!(
                std::arch::is_aarch64_feature_detected!("neon"),
                "CPU requires NEON"
            );
        }
        Ok(())
    }
    pub fn apply_affinity(&self, setting: &str) -> Result<()> {
        if setting == "off" {
            return Ok(());
        }
        let cpus = if setting == "auto" || setting == "on" {
            self.representatives.clone()
        } else {
            setting
                .split(',')
                .map(|s| {
                    s.trim()
                        .parse::<usize>()
                        .context("affinity must be auto, off, or comma-separated CPU indices")
                })
                .collect::<Result<Vec<_>>>()?
        };
        ensure!(!cpus.is_empty(), "empty requested affinity");
        let allowed: BTreeSet<_> = self.allowed.iter().copied().collect();
        // SAFETY: `cpu_set_t` is plain-old-data, so all-zero is a valid empty set ready for `CPU_SET`.
        let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for cpu in cpus {
            ensure!(
                allowed.contains(&cpu),
                "CPU {cpu} is outside process cpuset"
            );
            // SAFETY: `cpu` is in `allowed`, which was collected from indices below `CPU_SETSIZE`, and `mask` is a live stack value.
            unsafe {
                libc::CPU_SET(cpu, &mut mask);
            }
        }
        ensure!(
            // SAFETY: `mask` is the initialized stack affinity set and `size_of_val` passes exactly its size.
            unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&mask), &raw const mask) }
                == 0,
            "sched_setaffinity: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
}
pub fn lifecycle() -> Result<Arc<AtomicBool>> {
    // SAFETY: `getppid` takes no arguments and returns a scalar.
    let parent = unsafe { libc::getppid() };
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    ensure!(
        // SAFETY: `prctl` is called with scalar arguments only and its return is checked here.
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) } == 0,
        "PR_SET_PDEATHSIG failed"
    );
    // SAFETY: `getppid` takes no arguments and returns a scalar.
    if unsafe { libc::getppid() } != parent || parent == 1 {
        stop.store(true, Ordering::Relaxed);
    }
    Ok(stop)
}
pub fn maintenance(
    engine: Arc<std::sync::Mutex<Engine>>,
    cfg: &Config,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let trim = cfg.trim_interval;
    let runtime = engine
        .lock()
        .expect("engine mutex poisoned at startup")
        .runtime();
    std::thread::spawn(move || {
        let mut last_trim = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            let released = engine
                .try_lock()
                .is_ok_and(|mut engine| engine.release_idle());
            // glibc keeps freed arenas mapped; hand them back to the
            // OS right after the models go away.
            if released || trim.is_some_and(|d| last_trim.elapsed() >= d) {
                #[cfg(target_env = "gnu")]
                // SAFETY: `malloc_trim` takes only an integer and passes no pointers, so it cannot violate memory safety.
                unsafe {
                    libc::malloc_trim(0);
                }
                last_trim = Instant::now();
            }
        }
        runtime.cancel_active();
    })
}
#[derive(Clone, Copy)]
pub struct Resources {
    pub cpu_micros: u64,
    pub peak_rss_kib: i64,
}
pub fn resources() -> Resources {
    // SAFETY: `rusage` is plain-old-data, so all-zero is a valid value and any fields the kernel skips stay initialized.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a live stack out-parameter of the correct type; `getrusage` writes only into it.
    unsafe {
        libc::getrusage(libc::RUSAGE_SELF, &raw mut usage);
    }
    Resources {
        cpu_micros: ((usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64) * 1_000_000
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64,
        peak_rss_kib: usage.ru_maxrss as i64,
    }
}
