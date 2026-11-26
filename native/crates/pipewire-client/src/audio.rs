//! SPA raw-audio format negotiation primitives.
//!
//! These types intentionally describe the SPA wire format rather than an
//! application audio backend. They are shared by native `PipeWire` streams and
//! callers that need to negotiate a format without linking to libpipewire.

use std::io;

use pipewire_native_spa::{
    param::{
        buffers::Buffers as BuffersParam,
        format::{Format, MediaSubtype, MediaType},
        props::Prop,
        ParamType,
    },
    pod::{
        builder::Builder,
        parser::Parser,
        types::{Choice, Id, ObjectType, PropertyFlags, Type},
        RawPod,
    },
};

/// Interleaved raw PCM formats defined by `spa_audio_format`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSampleFormat {
    S16Le = 0x103,
    F32Le = 0x11b,
    /// Planar 32-bit float used by `PipeWire`'s per-channel DSP ports.
    F32P = 0x206,
}

impl From<AudioSampleFormat> for u32 {
    fn from(value: AudioSampleFormat) -> Self {
        value as Self
    }
}

impl TryFrom<u32> for AudioSampleFormat {
    type Error = u32;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0x103 => Ok(Self::S16Le),
            0x11b => Ok(Self::F32Le),
            0x206 => Ok(Self::F32P),
            other => Err(other),
        }
    }
}

/// Raw interleaved or planar DSP audio capabilities advertised by a local stream port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFormatCaps {
    pub default: AudioSampleFormat,
    pub alternatives: Vec<AudioSampleFormat>,
    pub rate: (u32, u32, u32),     // default, min, max
    pub channels: (u32, u32, u32), // default, min, max
}

impl Default for AudioFormatCaps {
    fn default() -> Self {
        Self {
            default: AudioSampleFormat::F32Le,
            alternatives: vec![AudioSampleFormat::F32Le, AudioSampleFormat::S16Le],
            rate: (48_000, 8_000, 192_000),
            channels: (2, 1, 2),
        }
    }
}

/// Fixed raw-audio format negotiated with the remote `PipeWire` node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedAudioFormat {
    pub sample_format: AudioSampleFormat,
    pub rate: u32,
    pub channels: u32,
}

/// Current node/device audio gain and mute state, when exposed by SPA Props.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioControls {
    /// Linear gain; 1.0 is unity and 0.0 is silence.
    pub volume: Option<f32>,
    pub mute: Option<bool>,
}

fn audio_position(channels: u32) -> Option<Vec<Id<u32>>> {
    match channels {
        1 => Some(vec![Id(2)]),        // SPA_AUDIO_CHANNEL_MONO
        2 => Some(vec![Id(3), Id(4)]), // SPA_AUDIO_CHANNEL_FL, FR
        _ => None,
    }
}

/// Encode SPA `EnumFormat` for raw audio or planar DSP audio.
pub fn encode_audio_format_caps(caps: &AudioFormatCaps) -> io::Result<Vec<u8>> {
    validate_audio_caps(caps)?;
    let mut buffer = [0_u8; 2048];
    let encoded = Builder::new(&mut buffer)
        .push_object(ObjectType::Format, ParamType::EnumFormat, |object| {
            let object = object
                .push_property(
                    Format::MediaType,
                    PropertyFlags::empty(),
                    Id(MediaType::Audio),
                )
                .push_property(
                    Format::MediaSubtype,
                    PropertyFlags::empty(),
                    Id(if caps.default == AudioSampleFormat::F32P {
                        MediaSubtype::Dsp
                    } else {
                        MediaSubtype::Raw
                    }),
                )
                .push_property(
                    Format::AudioFormat,
                    PropertyFlags::empty(),
                    Choice::Enum {
                        default: Id(caps.default),
                        alternatives: caps.alternatives.iter().copied().map(Id).collect(),
                    },
                )
                .push_property(
                    Format::AudioRate,
                    PropertyFlags::empty(),
                    Choice::Range {
                        default: caps.rate.0.cast_signed(),
                        min: caps.rate.1.cast_signed(),
                        max: caps.rate.2.cast_signed(),
                    },
                )
                .push_property(
                    Format::AudioChannels,
                    PropertyFlags::empty(),
                    Choice::Range {
                        default: caps.channels.0.cast_signed(),
                        min: caps.channels.1.cast_signed(),
                        max: caps.channels.2.cast_signed(),
                    },
                );
            if caps.channels.1 == caps.channels.2 {
                if let Some(position) = audio_position(caps.channels.0) {
                    return object.push_property(
                        Format::AudioPosition,
                        PropertyFlags::empty(),
                        position.as_slice(),
                    );
                }
            }
            object
        })
        .build()
        .map_err(pod_error("Encode SPA audio EnumFormat"))?;
    Ok(encoded.to_vec())
}

/// Encode SPA `Format` for one fixed raw or planar DSP audio format.
pub fn encode_negotiated_audio_format(format: NegotiatedAudioFormat) -> io::Result<Vec<u8>> {
    validate_audio_format(format)?;
    let mut buffer = [0_u8; 1024];
    let encoded = Builder::new(&mut buffer)
        .push_object(ObjectType::Format, ParamType::Format, |object| {
            let object = object
                .push_property(
                    Format::MediaType,
                    PropertyFlags::empty(),
                    Id(MediaType::Audio),
                )
                .push_property(
                    Format::MediaSubtype,
                    PropertyFlags::empty(),
                    Id(if format.sample_format == AudioSampleFormat::F32P {
                        MediaSubtype::Dsp
                    } else {
                        MediaSubtype::Raw
                    }),
                )
                .push_property(
                    Format::AudioFormat,
                    PropertyFlags::empty(),
                    Id(format.sample_format),
                )
                .push_property(
                    Format::AudioRate,
                    PropertyFlags::empty(),
                    format.rate.cast_signed(),
                )
                .push_property(
                    Format::AudioChannels,
                    PropertyFlags::empty(),
                    format.channels.cast_signed(),
                );
            if let Some(position) = audio_position(format.channels) {
                object.push_property(
                    Format::AudioPosition,
                    PropertyFlags::empty(),
                    position.as_slice(),
                )
            } else {
                object
            }
        })
        .build()
        .map_err(pod_error("Encode SPA audio Format"))?;
    Ok(encoded.to_vec())
}

/// Encode SPA `ParamBuffers` for interleaved audio with the negotiated frame size.
pub fn encode_audio_buffers_param(format: NegotiatedAudioFormat) -> io::Result<Vec<u8>> {
    validate_audio_format(format)?;
    let bytes_per_sample = match format.sample_format {
        AudioSampleFormat::S16Le => 2_u32,
        AudioSampleFormat::F32Le | AudioSampleFormat::F32P => 4_u32,
    };
    let stride = format
        .channels
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| invalid_input("Audio stride overflow"))?;
    let size = stride
        .checked_mul(format.rate / 50)
        .ok_or_else(|| invalid_input("Audio buffer size overflow"))?;
    let mut buffer = [0_u8; 2048];
    let encoded = Builder::new(&mut buffer)
        .push_object(ObjectType::ParamBuffers, ParamType::Buffers, |object| {
            object
                .push_property(
                    BuffersParam::Buffers,
                    PropertyFlags::empty(),
                    Choice::Range {
                        default: 8_i32,
                        min: 2_i32,
                        max: 32_i32,
                    },
                )
                .push_property(BuffersParam::Blocks, PropertyFlags::empty(), 1_i32)
                .push_property(
                    BuffersParam::Size,
                    PropertyFlags::empty(),
                    Choice::Range {
                        default: size.cast_signed(),
                        min: 1_i32,
                        max: size.cast_signed(),
                    },
                )
                .push_property(
                    BuffersParam::Stride,
                    PropertyFlags::empty(),
                    Choice::Range {
                        default: stride.cast_signed(),
                        min: stride.cast_signed(),
                        max: stride.cast_signed(),
                    },
                )
                .push_property(BuffersParam::Align, PropertyFlags::empty(), 16_i32)
                .push_property(
                    BuffersParam::DataType,
                    PropertyFlags::empty(),
                    Choice::Flags {
                        default: 30_i32,
                        flags: 30_i32,
                    },
                )
                .push_property(
                    BuffersParam::MetaType,
                    PropertyFlags::empty(),
                    Choice::Flags {
                        default: 128_i32,
                        flags: 128_i32,
                    },
                )
        })
        .build()
        .map_err(pod_error("Encode SPA audio ParamBuffers"))?;
    Ok(encoded.to_vec())
}

/// Decode a fixed SPA raw-audio or planar DSP `Format` POD.
pub fn decode_audio_format(pod: &RawPod<'_>) -> io::Result<NegotiatedAudioFormat> {
    decode_audio_format_with_defaults(pod, None)
}

/// Decode a SPA audio Format, using the requested fixed format for omitted optional fields.
pub fn decode_audio_format_with_defaults(
    pod: &RawPod<'_>,
    defaults: Option<NegotiatedAudioFormat>,
) -> io::Result<NegotiatedAudioFormat> {
    let (object_type, param_type, values) = Parser::new(pod.data())
        .pop_object_raw::<ParamType, _>(|properties, object_type, param_type| {
            let mut media_type = None;
            let mut media_subtype = None;
            let mut sample_format = None;
            let mut rate = None;
            let mut channels = None;
            while let Some((key, _, value)) = properties.pop_property()? {
                match key {
                    key if key == Format::MediaType as u32 => {
                        media_type = Some(decode_id::<MediaType>(&value).map_err(|error| {
                            pipewire_native_spa::pod::Error::Invalid(format!(
                                "MediaType: {error:?}"
                            ))
                        })?);
                    }
                    key if key == Format::MediaSubtype as u32 => {
                        media_subtype =
                            Some(decode_id::<MediaSubtype>(&value).map_err(|error| {
                                pipewire_native_spa::pod::Error::Invalid(format!(
                                    "MediaSubtype: {error:?}"
                                ))
                            })?);
                    }
                    key if key == Format::AudioFormat as u32 => {
                        sample_format =
                            Some(decode_audio_sample_format(&value).map_err(|error| {
                                pipewire_native_spa::pod::Error::Invalid(format!(
                                    "AudioFormat: {error:?}"
                                ))
                            })?);
                    }
                    key if key == Format::AudioRate as u32 => {
                        rate = Some(decode_int(&value).map_err(|error| {
                            pipewire_native_spa::pod::Error::Invalid(format!(
                                "AudioRate: {error:?}"
                            ))
                        })?);
                    }
                    key if key == Format::AudioChannels as u32 => {
                        channels = Some(decode_int(&value).map_err(|error| {
                            pipewire_native_spa::pod::Error::Invalid(format!(
                                "AudioChannels: {error:?}"
                            ))
                        })?);
                    }
                    _ => {}
                }
            }
            Ok((
                object_type,
                param_type,
                (media_type, media_subtype, sample_format, rate, channels),
            ))
        })
        .map(|value| value.0)
        .map_err(pod_error("Decode SPA audio Format"))?;

    if object_type != ObjectType::Format
        || param_type != ParamType::Format
        || values.0 != Some(MediaType::Audio)
        || !matches!(values.1, Some(MediaSubtype::Raw | MediaSubtype::Dsp))
    {
        return Err(invalid_data(
            "PipeWire sent an unsupported SPA audio Format",
        ));
    }
    let format = NegotiatedAudioFormat {
        sample_format: values
            .2
            .ok_or_else(|| invalid_data("SPA audio Format has no AudioFormat"))?,
        rate: values
            .3
            .or_else(|| defaults.map(|format| format.rate))
            .ok_or_else(|| {
                invalid_data("SPA audio Format has no AudioRate or requested default")
            })?,
        channels: values
            .4
            .or_else(|| defaults.map(|format| format.channels))
            .ok_or_else(|| {
                invalid_data("SPA audio Format has no AudioChannels or requested default")
            })?,
    };
    validate_audio_format(format)?;
    if values.1 == Some(MediaSubtype::Dsp) && format.sample_format != AudioSampleFormat::F32P {
        return Err(invalid_data(
            "PipeWire DSP audio format is not planar float",
        ));
    }
    if values.1 == Some(MediaSubtype::Raw) && format.sample_format == AudioSampleFormat::F32P {
        return Err(invalid_data(
            "PipeWire planar float format must use DSP media subtype",
        ));
    }
    Ok(format)
}

/// Decode a native SPA Props POD, ignoring unrelated properties.
pub fn decode_audio_controls(pod: &RawPod<'_>) -> io::Result<AudioControls> {
    let (object_type, param_type, controls) = Parser::new(pod.data())
        .pop_object_raw::<ParamType, _>(|properties, object_type, param_type| {
            let mut controls = AudioControls {
                volume: None,
                mute: None,
            };
            while let Some((key, _, value)) = properties.pop_property()? {
                match key {
                    key if key == Prop::Volume as u32 => {
                        controls.volume = Some(value.decode::<f32>()?);
                    }
                    key if key == Prop::Mute as u32 => {
                        controls.mute = Some(value.decode::<bool>()?);
                    }
                    _ => {}
                }
            }
            Ok((object_type, param_type, controls))
        })
        .map(|value| value.0)
        .map_err(pod_error("Decode SPA audio Props"))?;
    if object_type != ObjectType::Props || param_type != ParamType::Props {
        return Err(invalid_data("PipeWire parameter is not SPA Props"));
    }
    if controls
        .volume
        .is_some_and(|volume| !volume.is_finite() || volume < 0.0)
    {
        return Err(invalid_data("PipeWire returned an invalid audio volume"));
    }
    Ok(controls)
}

fn validate_audio_caps(caps: &AudioFormatCaps) -> io::Result<()> {
    if caps.alternatives.is_empty()
        || !caps.alternatives.contains(&caps.default)
        || caps.rate.1 == 0
        || caps.rate.1 > caps.rate.0
        || caps.rate.0 > caps.rate.2
        || caps.channels.1 == 0
        || caps.channels.1 > caps.channels.0
        || caps.channels.0 > caps.channels.2
        || caps.channels.2 > 64
        || caps.rate.2 > 768_000
    {
        return Err(invalid_input("Invalid SPA raw-audio format capabilities"));
    }
    Ok(())
}

fn validate_audio_format(format: NegotiatedAudioFormat) -> io::Result<()> {
    if !(8_000..=768_000).contains(&format.rate) || !(1..=64).contains(&format.channels) {
        return Err(invalid_data(
            "PipeWire negotiated invalid raw-audio parameters",
        ));
    }
    Ok(())
}

fn decode_audio_sample_format(
    pod: &RawPod<'_>,
) -> Result<AudioSampleFormat, pipewire_native_spa::pod::Error> {
    let value = match pod.type_() {
        Type::Id => pod.decode::<Id<u32>>().map(|value| value.0)?,
        Type::Choice => {
            let choice = pod.decode::<Choice<Id<u32>>>()?;
            match choice {
                Choice::None(value)
                | Choice::Range { default: value, .. }
                | Choice::Step { default: value, .. }
                | Choice::Enum { default: value, .. }
                | Choice::Flags { default: value, .. } => value.0,
            }
        }
        other => {
            return Err(pipewire_native_spa::pod::Error::Invalid(format!(
                "Unexpected SPA AudioFormat POD type {other:?}"
            )));
        }
    };
    AudioSampleFormat::try_from(value).map_err(|other| {
        pipewire_native_spa::pod::Error::Invalid(format!(
            "Unsupported SPA audio sample format {other}"
        ))
    })
}

fn decode_id<T>(pod: &RawPod<'_>) -> Result<T, pipewire_native_spa::pod::Error>
where
    T: TryFrom<u32>,
{
    let raw = if let Ok(value) = pod.decode::<Id<u32>>() {
        value.0
    } else {
        let choice = pod.decode::<Choice<Id<u32>>>()?;
        match choice {
            Choice::None(value)
            | Choice::Range { default: value, .. }
            | Choice::Step { default: value, .. }
            | Choice::Enum { default: value, .. }
            | Choice::Flags { default: value, .. } => value.0,
        }
    };
    T::try_from(raw)
        .map_err(|_| pipewire_native_spa::pod::Error::Invalid(format!("Unexpected SPA ID {raw}")))
}

fn decode_int(pod: &RawPod<'_>) -> Result<u32, pipewire_native_spa::pod::Error> {
    let value = if let Ok(value) = pod.decode::<i32>() {
        value
    } else {
        let choice = pod.decode::<Choice<i32>>()?;
        match choice {
            Choice::None(value)
            | Choice::Range { default: value, .. }
            | Choice::Step { default: value, .. }
            | Choice::Enum { default: value, .. }
            | Choice::Flags { default: value, .. } => value,
        }
    };
    u32::try_from(value)
        .map_err(|_| pipewire_native_spa::pod::Error::Invalid("Negative SPA audio integer".into()))
}

fn pod_error(context: &'static str) -> impl FnOnce(pipewire_native_spa::pod::Error) -> io::Error {
    move |error| io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {error:?}"))
}
fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_controls_decode_volume_and_mute_props() {
        let mut buffer = [0_u8; 256];
        let encoded = Builder::new(&mut buffer)
            .push_object(ObjectType::Props, ParamType::Props, |object| {
                object
                    .push_property(Prop::Volume, PropertyFlags::empty(), 0.75_f32)
                    .push_property(Prop::Mute, PropertyFlags::empty(), true)
            })
            .build()
            .unwrap();
        let pod = RawPod::wrap(encoded).unwrap();
        assert_eq!(
            decode_audio_controls(&pod).unwrap(),
            AudioControls {
                volume: Some(0.75),
                mute: Some(true),
            }
        );
    }

    #[test]
    fn audio_format_caps_encode_and_fixed_format_round_trips() {
        let caps = AudioFormatCaps::default();
        let enum_bytes = encode_audio_format_caps(&caps).unwrap();
        let enum_pod = RawPod::wrap(&enum_bytes).unwrap();
        assert_eq!(enum_pod.type_(), Type::Object);

        let format = NegotiatedAudioFormat {
            sample_format: AudioSampleFormat::S16Le,
            rate: 48_000,
            channels: 2,
        };
        let bytes = encode_negotiated_audio_format(format).unwrap();
        let pod = RawPod::wrap(&bytes).unwrap();
        assert_eq!(decode_audio_format(&pod).unwrap(), format);
        let buffers = encode_audio_buffers_param(format).unwrap();
        let buffers_pod = RawPod::wrap(&buffers).unwrap();
        assert_eq!(buffers_pod.type_(), Type::Object);
    }

    #[test]
    fn dsp_planar_float_format_round_trips() {
        let caps = AudioFormatCaps {
            default: AudioSampleFormat::F32P,
            alternatives: vec![AudioSampleFormat::F32P],
            rate: (48_000, 48_000, 48_000),
            channels: (1, 1, 1),
        };
        let enum_bytes = encode_audio_format_caps(&caps).unwrap();
        assert_eq!(RawPod::wrap(&enum_bytes).unwrap().type_(), Type::Object);

        let format = NegotiatedAudioFormat {
            sample_format: AudioSampleFormat::F32P,
            rate: 48_000,
            channels: 1,
        };
        let bytes = encode_negotiated_audio_format(format).unwrap();
        assert_eq!(
            decode_audio_format(&RawPod::wrap(&bytes).unwrap()).unwrap(),
            format
        );
        assert_eq!(
            RawPod::wrap(&encode_audio_buffers_param(format).unwrap())
                .unwrap()
                .type_(),
            Type::Object
        );
    }

    #[test]
    fn rejects_invalid_audio_ranges_and_negotiated_values() {
        let caps = AudioFormatCaps {
            rate: (48_000, 96_000, 192_000),
            ..AudioFormatCaps::default()
        };
        assert!(encode_audio_format_caps(&caps).is_err());
        assert!(encode_negotiated_audio_format(NegotiatedAudioFormat {
            sample_format: AudioSampleFormat::F32Le,
            rate: 0,
            channels: 2,
        })
        .is_err());
    }
}
