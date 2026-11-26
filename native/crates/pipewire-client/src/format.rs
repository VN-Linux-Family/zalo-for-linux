//! SPA parameter PODs and well-known video format values.

use std::io;

use pipewire_native_spa::{
    param::{
        buffers::{Buffers as BuffersParam, Io, Meta},
        format::{Format, MediaSubtype, MediaType},
        ParamType,
    },
    pod::{
        builder::Builder,
        parser::Parser,
        types::{Choice, Id, ObjectType, PropertyFlags, Rectangle, Type},
        RawPod,
    },
};

/// Raw-video pixel formats from SPA's `spa_video_format` ABI.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    Yuy2 = 4,
    Uyvy = 5,
    Bgrx = 8,
    Bgra = 12,
}

impl VideoFormat {
    /// Bytes in one packed pixel pair row unit for the supported SPA formats.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Yuy2 | Self::Uyvy => 2,
            Self::Bgrx | Self::Bgra => 4,
        }
    }
}

impl From<VideoFormat> for u32 {
    fn from(value: VideoFormat) -> Self {
        value as Self
    }
}

impl TryFrom<u32> for VideoFormat {
    type Error = u32;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            4 => Ok(Self::Yuy2),
            5 => Ok(Self::Uyvy),
            8 => Ok(Self::Bgrx),
            12 => Ok(Self::Bgra),
            other => Err(other),
        }
    }
}

/// Inclusive size bounds for a raw video format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSizeRange {
    pub default: (u32, u32),
    pub min: (u32, u32),
    pub max: (u32, u32),
}

/// Capabilities advertised by a local video input port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFormatCaps {
    pub default: VideoFormat,
    /// The SPA Choice Enum alternatives, including the default value.
    pub alternatives: Vec<VideoFormat>,
    pub size: VideoSizeRange,
}

impl Default for VideoFormatCaps {
    fn default() -> Self {
        Self {
            default: VideoFormat::Bgra,
            alternatives: vec![VideoFormat::Bgra, VideoFormat::Bgrx],
            size: VideoSizeRange {
                default: (1920, 1080),
                min: (320, 240),
                max: (1920, 1080),
            },
        }
    }
}

/// The fixed format selected by the remote node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedVideoFormat {
    pub pixel_format: VideoFormat,
    pub width: u32,
    pub height: u32,
}

/// Encode one SPA `EnumFormat` object for raw video.
pub fn encode_video_format_caps(caps: &VideoFormatCaps) -> io::Result<Vec<u8>> {
    let mut buffer = [0_u8; 4096];
    let encoded = Builder::new(&mut buffer)
        .push_object(ObjectType::Format, ParamType::EnumFormat, |object| {
            object
                .push_property(
                    Format::MediaType,
                    PropertyFlags::empty(),
                    Id(MediaType::Video),
                )
                .push_property(
                    Format::MediaSubtype,
                    PropertyFlags::empty(),
                    Id(MediaSubtype::Raw),
                )
                .push_property(
                    Format::VideoFormat,
                    PropertyFlags::empty(),
                    Choice::Enum {
                        default: Id(caps.default),
                        alternatives: caps.alternatives.iter().copied().map(Id).collect(),
                    },
                )
                .push_property(
                    Format::VideoSize,
                    PropertyFlags::empty(),
                    Choice::Range {
                        default: Rectangle {
                            width: caps.size.default.0,
                            height: caps.size.default.1,
                        },
                        min: Rectangle {
                            width: caps.size.min.0,
                            height: caps.size.min.1,
                        },
                        max: Rectangle {
                            width: caps.size.max.0,
                            height: caps.size.max.1,
                        },
                    },
                )
        })
        .build()
        .map_err(pod_error("Encode SPA video EnumFormat"))?;
    Ok(encoded.to_vec())
}

/// Encode one fixed SPA Format selected by a remote node.
pub fn encode_negotiated_video_format(format: NegotiatedVideoFormat) -> io::Result<Vec<u8>> {
    let mut buffer = [0_u8; 1024];
    let encoded = Builder::new(&mut buffer)
        .push_object(ObjectType::Format, ParamType::Format, |object| {
            object
                .push_property(
                    Format::MediaType,
                    PropertyFlags::empty(),
                    Id(MediaType::Video),
                )
                .push_property(
                    Format::MediaSubtype,
                    PropertyFlags::empty(),
                    Id(MediaSubtype::Raw),
                )
                .push_property(
                    Format::VideoFormat,
                    PropertyFlags::empty(),
                    Id(format.pixel_format),
                )
                .push_property(
                    Format::VideoSize,
                    PropertyFlags::empty(),
                    Rectangle {
                        width: format.width,
                        height: format.height,
                    },
                )
        })
        .build()
        .map_err(pod_error("Encode SPA video Format"))?;
    Ok(encoded.to_vec())
}

/// Decode the fixed SPA Format selected by the `PipeWire` remote.
pub fn decode_video_format(pod: &RawPod<'_>) -> io::Result<NegotiatedVideoFormat> {
    let mut parser = Parser::new(pod.data());
    let (object_type, param_type, negotiated) = parser
        .pop_object_raw::<ParamType, _>(|properties, object_type, param_type| {
            let mut pixel_format = None;
            let mut size = None;
            while let Some((key, _, value)) = properties.pop_property()? {
                match key {
                    key if key == Format::VideoFormat as u32 => {
                        pixel_format = Some(decode_format_id(&value)?);
                    }
                    key if key == Format::VideoSize as u32 => {
                        size = Some(decode_rectangle(&value)?);
                    }
                    _ => {}
                }
            }
            Ok((object_type, param_type, (pixel_format, size)))
        })
        .map(|value| value.0)
        .map_err(pod_error("Decode SPA video Format"))?;

    if object_type != ObjectType::Format || param_type != ParamType::Format {
        return Err(invalid_data("PipeWire sent a non-Format SPA object"));
    }
    let pixel_format = negotiated
        .0
        .ok_or_else(|| invalid_data("SPA Format does not contain VideoFormat"))?;
    let size = negotiated
        .1
        .ok_or_else(|| invalid_data("SPA Format does not contain VideoSize"))?;
    if size.width == 0 || size.height == 0 || size.width > 8192 || size.height > 8192 {
        return Err(invalid_data("PipeWire negotiated an invalid video size"));
    }

    Ok(NegotiatedVideoFormat {
        pixel_format,
        width: size.width,
        height: size.height,
    })
}

fn decode_format_id(pod: &RawPod<'_>) -> Result<VideoFormat, pipewire_native_spa::pod::Error> {
    match pod.type_() {
        Type::Id => pod.decode::<Id<VideoFormat>>().map(|id| id.0),
        Type::Choice => {
            let choice = pod.decode::<Choice<Id<VideoFormat>>>()?;
            let id = match choice {
                Choice::None(value)
                | Choice::Range { default: value, .. }
                | Choice::Step { default: value, .. }
                | Choice::Enum { default: value, .. }
                | Choice::Flags { default: value, .. } => value,
            };
            Ok(id.0)
        }
        other => Err(pipewire_native_spa::pod::Error::Invalid(format!(
            "Unexpected SPA VideoFormat POD type {other:?}"
        ))),
    }
}

fn decode_rectangle(pod: &RawPod<'_>) -> Result<Rectangle, pipewire_native_spa::pod::Error> {
    match pod.type_() {
        Type::Rectangle => pod.decode::<Rectangle>(),
        Type::Choice => {
            let choice = pod.decode::<Choice<Rectangle>>()?;
            Ok(match choice {
                Choice::None(value)
                | Choice::Range { default: value, .. }
                | Choice::Step { default: value, .. }
                | Choice::Enum { default: value, .. }
                | Choice::Flags { default: value, .. } => value,
            })
        }
        other => Err(pipewire_native_spa::pod::Error::Invalid(format!(
            "Unexpected SPA VideoSize POD type {other:?}"
        ))),
    }
}

/// Encode SPA `ParamBuffers` for one supported packed raw-video plane.
pub fn encode_video_buffers_param(format: NegotiatedVideoFormat) -> io::Result<Vec<u8>> {
    let stride = format
        .width
        .checked_mul(format.pixel_format.bytes_per_pixel())
        .ok_or_else(|| invalid_input("Video stride overflow"))?;
    let size = stride
        .checked_mul(format.height)
        .ok_or_else(|| invalid_input("Video buffer size overflow"))?;
    let size = size.cast_signed();
    let stride = stride.cast_signed();

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
                        default: size,
                        min: 1_i32,
                        max: size,
                    },
                )
                .push_property(
                    BuffersParam::Stride,
                    PropertyFlags::empty(),
                    Choice::Range {
                        default: stride,
                        min: stride,
                        max: stride,
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
                        default: 0_i32,
                        flags: 0_i32,
                    },
                )
        })
        .build()
        .map_err(pod_error("Encode SPA ParamBuffers"))?;
    Ok(encoded.to_vec())
}

/// Encode `SPA_PARAM_IO` for `SPA_IO_Buffers`, whose ABI size is 8 bytes.
pub fn encode_buffers_io_param() -> io::Result<Vec<u8>> {
    let mut buffer = [0_u8; 256];
    let encoded = Builder::new(&mut buffer)
        .push_object(ObjectType::ParamIo, ParamType::IO, |object| {
            object
                .push_property(Io::Id, PropertyFlags::empty(), Id(1_u32))
                .push_property(Io::Size, PropertyFlags::empty(), 8_i32)
        })
        .build()
        .map_err(pod_error("Encode SPA_PARAM_IO Buffers"))?;
    Ok(encoded.to_vec())
}

/// Encode `SPA_PARAM_Meta` for the Busy metadata, which is two uint32 fields.
pub fn encode_busy_meta_param() -> io::Result<Vec<u8>> {
    let mut buffer = [0_u8; 256];
    let encoded = Builder::new(&mut buffer)
        .push_object(ObjectType::ParamMeta, ParamType::Meta, |object| {
            object
                .push_property(Meta::Type, PropertyFlags::empty(), Id(7_u32))
                .push_property(Meta::Size, PropertyFlags::empty(), 8_i32)
        })
        .build()
        .map_err(pod_error("Encode SPA_PARAM_Meta Busy"))?;
    Ok(encoded.to_vec())
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
    use pipewire_native_spa::pod::parser::Parser;

    #[test]
    fn video_caps_preserve_spa_abi_values_and_properties() {
        let encoded = encode_video_format_caps(&VideoFormatCaps::default()).unwrap();
        let mut parser = Parser::new(&encoded);
        let (object_type, param_type, fields) = parser
            .pop_object_raw::<ParamType, _>(|properties, object_type, param_type| {
                let mut fields = Vec::new();
                while let Some((key, _, value)) = properties.pop_property()? {
                    fields.push((key, value.data().to_vec()));
                }
                Ok((object_type, param_type, fields))
            })
            .unwrap()
            .0;

        assert_eq!(object_type, ObjectType::Format);
        assert_eq!(param_type, ParamType::EnumFormat);
        assert_eq!(
            fields.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            vec![
                Format::MediaType as u32,
                Format::MediaSubtype as u32,
                Format::VideoFormat as u32,
                Format::VideoSize as u32,
            ]
        );
        assert_eq!(VideoFormat::try_from(12), Ok(VideoFormat::Bgra));
        assert_eq!(VideoFormat::try_from(8), Ok(VideoFormat::Bgrx));
    }

    #[test]
    fn buffer_io_and_busy_meta_parameters_are_valid_spa_objects() {
        let io_param = encode_buffers_io_param().unwrap();
        let mut io_parser = Parser::new(&io_param);
        let io_type = io_parser.pop_object_raw::<ParamType, _>(|properties, _, id| {
            let entries = properties.count();
            Ok((id, entries))
        });
        assert!(io_type.is_ok());

        let meta_param = encode_busy_meta_param().unwrap();
        let mut meta_parser = Parser::new(&meta_param);
        let meta_type = meta_parser.pop_object_raw::<ParamType, _>(|properties, _, id| {
            let entries = properties.count();
            Ok((id, entries))
        });
        assert!(meta_type.is_ok());
    }

    #[test]
    fn negotiated_video_format_round_trips_as_spa_format() {
        let format = NegotiatedVideoFormat {
            pixel_format: VideoFormat::Bgra,
            width: 1280,
            height: 720,
        };
        let bytes = encode_negotiated_video_format(format).unwrap();
        let pod = RawPod::wrap(&bytes).unwrap();
        assert_eq!(decode_video_format(&pod).unwrap(), format);
    }

    #[test]
    fn video_format_decodes_fixed_and_choice_shaped_spa_values() {
        let negotiated = NegotiatedVideoFormat {
            pixel_format: VideoFormat::Bgra,
            width: 1280,
            height: 720,
        };
        let mut buffer = [0_u8; 1024];
        let format = Builder::new(&mut buffer)
            .push_object(ObjectType::Format, ParamType::Format, |object| {
                object
                    .push_property(
                        Format::VideoFormat,
                        PropertyFlags::empty(),
                        Id(negotiated.pixel_format),
                    )
                    .push_property(
                        Format::VideoSize,
                        PropertyFlags::empty(),
                        Rectangle {
                            width: negotiated.width,
                            height: negotiated.height,
                        },
                    )
            })
            .build()
            .unwrap()
            .to_vec();
        let pod = RawPod::wrap(&format).unwrap();
        assert_eq!(decode_video_format(&pod).unwrap(), negotiated);

        let buffers = encode_video_buffers_param(negotiated).unwrap();
        let mut parser = Parser::new(&buffers);
        let (object, param, keys) = parser
            .pop_object_raw::<ParamType, _>(|properties, object, param| {
                let keys = properties.map(|(key, _, _)| key).collect::<Vec<_>>();
                Ok((object, param, keys))
            })
            .unwrap()
            .0;
        assert_eq!(object, ObjectType::ParamBuffers);
        assert_eq!(param, ParamType::Buffers);
        assert_eq!(
            keys,
            vec![
                BuffersParam::Buffers as u32,
                BuffersParam::Blocks as u32,
                BuffersParam::Size as u32,
                BuffersParam::Stride as u32,
                BuffersParam::Align as u32,
                BuffersParam::DataType as u32,
                BuffersParam::MetaType as u32,
            ]
        );
    }
}
