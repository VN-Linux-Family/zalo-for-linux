//! Audio backend selection and public compatibility facade.

/// Audio output selected using the same Pulse/PipeWire preference as capture.
pub enum AudioPlayer {
    Pulse(super::pulse::PulsePlayer),
    PipeWire(Box<super::pipewire::PipeWirePlayer>),
}

impl AudioPlayer {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        select_audio_backend(
            selected_backend(),
            "audio playback",
            || super::pulse::PulsePlayer::new(device).map(Self::Pulse),
            || {
                super::pipewire::PipeWirePlayer::new(device)
                    .map(Box::new)
                    .map(Self::PipeWire)
            },
        )
    }

    pub fn write(&mut self, buffer: &[u8]) -> Result<(), String> {
        match self {
            Self::Pulse(player) => player.write(buffer),
            Self::PipeWire(player) => player.write(buffer),
        }
    }

    #[must_use]
    pub fn latency_ms(&self) -> Option<u64> {
        match self {
            Self::Pulse(player) => player.latency_ms(),
            Self::PipeWire(_) => super::pipewire::PipeWirePlayer::latency_ms(),
        }
    }
}

/// Audio input implementation selected by device preferences.
pub enum AudioRecorder {
    Pulse(super::pulse::PulseRecorder),
    PipeWire(Box<super::pipewire::PipeWireRecorder>),
}

impl AudioRecorder {
    /// Latest microphone PCM meter from the selected capture backend.
    #[must_use]
    pub fn microphone_meter(&self) -> Option<&super::meter::AudioMeter> {
        match self {
            Self::PipeWire(recorder) => recorder.last_meter(),
            Self::Pulse(recorder) => recorder.last_meter(),
        }
    }

    /// Backward-compatible PipeWire-specific meter accessor.
    #[must_use]
    pub fn pipewire_meter(&self) -> Option<&super::meter::AudioMeter> {
        match self {
            Self::PipeWire(recorder) => recorder.last_meter(),
            Self::Pulse(_) => None,
        }
    }

    pub fn new(device: Option<&str>) -> Result<Self, String> {
        use crate::devices::load_prefs;
        select_audio_backend(
            load_prefs().audio_backend,
            "audio capture",
            || super::pulse::PulseRecorder::new(device).map(Self::Pulse),
            || {
                super::pipewire::PipeWireRecorder::new(device)
                    .map(Box::new)
                    .map(Self::PipeWire)
            },
        )
    }

    pub fn read(&mut self, output: &mut [u8]) -> Result<(), String> {
        match self {
            Self::Pulse(recorder) => recorder.read(output),
            Self::PipeWire(recorder) => recorder.read(output),
        }
    }
}

pub use super::pipewire::PipeWireRecorder;
pub use super::pulse::{
    probe_audio_hardware, PulseAudioDevice, PulseDeviceInventory, PulsePlayer, PulseRecorder,
    PulseSourceInfo,
};

fn select_audio_backend<T>(
    backend: crate::devices::AudioBackend,
    purpose: &str,
    pulse: impl FnOnce() -> Result<T, String>,
    pipewire: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match backend {
        crate::devices::AudioBackend::Pulse => pulse(),
        crate::devices::AudioBackend::PipeWire => pipewire(),
        crate::devices::AudioBackend::Auto => auto_fallback(purpose, pulse, pipewire),
    }
}

fn auto_fallback<T>(
    purpose: &str,
    pulse: impl FnOnce() -> Result<T, String>,
    pipewire: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    pulse().or_else(|pulse_error| {
        pipewire().map_err(|pipewire_error| {
            format!("{purpose} auto-detect failed (PulseAudio: {pulse_error}; PipeWire: {pipewire_error})")
        })
    })
}

fn selected_backend() -> crate::devices::AudioBackend {
    crate::devices::load_prefs().audio_backend
}

/// Read microphone volume/mute using the selected backend, with compatibility fallback.
#[must_use]
pub fn pulse_get_source_info(source: Option<&str>) -> Option<PulseSourceInfo> {
    match selected_backend() {
        crate::devices::AudioBackend::PipeWire => super::pipewire::pipewire_get_source_info(source),
        crate::devices::AudioBackend::Pulse => super::pulse::pulse_get_source_info(source),
        crate::devices::AudioBackend::Auto => super::pulse::pulse_get_source_info(source)
            .or_else(|| super::pipewire::pipewire_get_source_info(source)),
    }
}

#[must_use]
pub fn get_source_volume(source: Option<&str>) -> Option<u8> {
    pulse_get_source_info(source).map(|info| info.pct)
}

pub fn set_source_volume(source: Option<&str>, percent: u8) -> Result<(), String> {
    match selected_backend() {
        crate::devices::AudioBackend::PipeWire => {
            super::pipewire::pipewire_set_source_volume(source, percent)
        }
        crate::devices::AudioBackend::Pulse => super::pulse::set_source_volume(source, percent),
        crate::devices::AudioBackend::Auto => super::pulse::set_source_volume(source, percent)
            .or_else(|_| super::pipewire::pipewire_set_source_volume(source, percent)),
    }
}

pub fn set_source_mute(source: Option<&str>, mute: bool) -> Result<(), String> {
    match selected_backend() {
        crate::devices::AudioBackend::PipeWire => {
            super::pipewire::pipewire_set_source_mute(source, mute)
        }
        crate::devices::AudioBackend::Pulse => super::pulse::set_source_mute(source, mute),
        crate::devices::AudioBackend::Auto => super::pulse::set_source_mute(source, mute)
            .or_else(|_| super::pipewire::pipewire_set_source_mute(source, mute)),
    }
}

/// Read output sink volume/mute using native `PipeWire` controls.
#[must_use]
pub fn get_sink_info(sink: Option<&str>) -> Option<PulseSourceInfo> {
    match selected_backend() {
        crate::devices::AudioBackend::PipeWire => super::pipewire::pipewire_get_sink_info(sink),
        crate::devices::AudioBackend::Pulse => super::pulse::pulse_get_sink_info(sink),
        crate::devices::AudioBackend::Auto => super::pulse::pulse_get_sink_info(sink)
            .or_else(|| super::pipewire::pipewire_get_sink_info(sink)),
    }
}

pub fn set_sink_volume(sink: Option<&str>, percent: u8) -> Result<(), String> {
    match selected_backend() {
        crate::devices::AudioBackend::PipeWire => {
            super::pipewire::pipewire_set_sink_volume(sink, percent)
        }
        crate::devices::AudioBackend::Pulse => super::pulse::set_sink_volume(sink, percent),
        crate::devices::AudioBackend::Auto => super::pulse::set_sink_volume(sink, percent)
            .or_else(|_| super::pipewire::pipewire_set_sink_volume(sink, percent)),
    }
}

pub fn set_sink_mute(sink: Option<&str>, mute: bool) -> Result<(), String> {
    match selected_backend() {
        crate::devices::AudioBackend::PipeWire => {
            super::pipewire::pipewire_set_sink_mute(sink, mute)
        }
        crate::devices::AudioBackend::Pulse => super::pulse::set_sink_mute(sink, mute),
        crate::devices::AudioBackend::Auto => super::pulse::set_sink_mute(sink, mute)
            .or_else(|_| super::pipewire::pipewire_set_sink_mute(sink, mute)),
    }
}

/// Normalize the current process capture stream for the PulseAudio-compatible backend.
pub fn pulse_normalize_own_stream() {
    if selected_backend() != crate::devices::AudioBackend::PipeWire {
        super::pulse::pulse_normalize_own_stream();
    }
}

/// Enumerate devices through the selected native backend, falling back to `PulseAudio`.
#[must_use]
pub fn pulse_device_inventory() -> Option<PulseDeviceInventory> {
    match selected_backend() {
        crate::devices::AudioBackend::PipeWire => super::pipewire::pipewire_device_inventory(),
        crate::devices::AudioBackend::Pulse => super::pulse::pulse_device_inventory(),
        crate::devices::AudioBackend::Auto => super::pulse::pulse_device_inventory()
            .or_else(super::pipewire::pipewire_device_inventory),
    }
}

#[cfg(test)]
mod tests {
    use super::select_audio_backend;
    use crate::devices::AudioBackend;
    use std::cell::RefCell;

    #[test]
    fn explicit_pulse_never_probes_pipewire() {
        let calls = RefCell::new(Vec::new());
        let selected = select_audio_backend(
            AudioBackend::Pulse,
            "audio capture",
            || {
                calls.borrow_mut().push("pulse");
                Ok("Pulse")
            },
            || {
                calls.borrow_mut().push("pipewire");
                Ok("PipeWire")
            },
        )
        .unwrap();
        assert_eq!(selected, "Pulse");
        assert_eq!(*calls.borrow(), ["pulse"]);
    }

    #[test]
    fn explicit_pipewire_never_probes_pulse() {
        let calls = RefCell::new(Vec::new());
        let selected = select_audio_backend(
            AudioBackend::PipeWire,
            "audio playback",
            || {
                calls.borrow_mut().push("pulse");
                Ok("Pulse")
            },
            || {
                calls.borrow_mut().push("pipewire");
                Ok("PipeWire")
            },
        )
        .unwrap();
        assert_eq!(selected, "PipeWire");
        assert_eq!(*calls.borrow(), ["pipewire"]);
    }

    #[test]
    fn auto_prefers_pulse_and_does_not_probe_pipewire_on_success() {
        let calls = RefCell::new(Vec::new());
        let selected = select_audio_backend(
            AudioBackend::Auto,
            "audio playback",
            || {
                calls.borrow_mut().push("pulse");
                Ok("Pulse")
            },
            || {
                calls.borrow_mut().push("pipewire");
                Ok("PipeWire")
            },
        )
        .unwrap();
        assert_eq!(selected, "Pulse");
        assert_eq!(*calls.borrow(), ["pulse"]);
    }

    #[test]
    fn auto_falls_back_to_pipewire_only_after_pulse_fails() {
        let calls = RefCell::new(Vec::new());
        let selected = select_audio_backend(
            AudioBackend::Auto,
            "audio capture",
            || {
                calls.borrow_mut().push("pulse");
                Err("Pulse unavailable".into())
            },
            || {
                calls.borrow_mut().push("pipewire");
                Ok("PipeWire")
            },
        )
        .unwrap();
        assert_eq!(selected, "PipeWire");
        assert_eq!(*calls.borrow(), ["pulse", "pipewire"]);
    }

    #[test]
    fn auto_reports_both_backend_failures() {
        let error = select_audio_backend::<()>(
            AudioBackend::Auto,
            "audio playback",
            || Err("Pulse unavailable".into()),
            || Err("PipeWire unavailable".into()),
        )
        .unwrap_err();
        assert!(error.contains("audio playback auto-detect failed"));
        assert!(error.contains("PulseAudio: Pulse unavailable"));
        assert!(error.contains("PipeWire: PipeWire unavailable"));
    }
}
