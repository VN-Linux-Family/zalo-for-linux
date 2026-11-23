use crate::engine::geometry::{distance, project, Quad};
use crate::ipc::protocol::RawHeader;
use anyhow::{ensure, Context, Result};
use image::{
    imageops::{self, FilterType},
    ImageReader, Rgb, RgbImage,
};
use std::{
    io::{Cursor, Read},
    path::Path,
    time::Instant,
};
#[allow(
    clippy::struct_field_names,
    reason = "all three timings share the same unit suffix"
)]
pub struct InputTimings {
    pub read_micros: u128,
    pub decode_micros: u128,
    pub color_micros: u128,
}
pub fn read_profiled(path: &Path) -> Result<(RgbImage, InputTimings)> {
    let start = Instant::now();
    let file = std::fs::File::open(path).context("unable to read image")?;
    ensure!(
        file.metadata()?.len() <= 134_217_728,
        "compressed image exceeds 128 MiB"
    );
    let mut bytes = Vec::new();
    file.take(134_217_729).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 134_217_728,
        "compressed image exceeds 128 MiB"
    );
    let read_micros = start.elapsed().as_micros();
    let start = Instant::now();
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(30000);
    limits.max_image_height = Some(30000);
    limits.max_alloc = Some(134_217_728);
    reader.limits(limits);
    let image = reader.decode().context("image decode failed")?;
    ensure!(
        u64::from(image.width()) * u64::from(image.height()) * 3 <= 134_217_728,
        "decoded image exceeds 128 MiB"
    );
    let decode_micros = start.elapsed().as_micros();
    let start = Instant::now();
    let image = image.into_rgb8();
    let color_micros = start.elapsed().as_micros();
    Ok((
        image,
        InputTimings {
            read_micros,
            decode_micros,
            color_micros,
        },
    ))
}
pub fn raw(header: &RawHeader, data: &[u8]) -> Result<RgbImage> {
    ensure!(data.len() == header.size, "raw buffer length mismatch");
    let mut image = RgbImage::new(header.width, header.height);
    for (y, row) in data.chunks_exact(header.stride as usize).enumerate() {
        for (x, p) in row[..header.width as usize * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
        {
            let rgb = if header.format == 1 {
                [p[0], p[1], p[2]]
            } else {
                [p[2], p[1], p[0]]
            };
            image.put_pixel(x as u32, y as u32, Rgb(rgb));
        }
    }
    Ok(image)
}
pub fn cap(image: &RgbImage, max: u32) -> RgbImage {
    let ratio = (f64::from(max) / f64::from(image.width().max(image.height()))).min(1.0);
    if ratio >= 1.0 {
        return image.clone();
    }
    imageops::resize(
        image,
        (f64::from(image.width()) * ratio).round().max(1.) as u32,
        (f64::from(image.height()) * ratio).round().max(1.) as u32,
        FilterType::Triangle,
    )
}
pub fn detection(image: &RgbImage) -> RgbImage {
    let w = image.width().div_ceil(32) * 32;
    let h = image.height().div_ceil(32) * 32;
    imageops::resize(image, w, h, FilterType::Triangle)
}
pub fn tensor(image: &RgbImage, detection: bool) -> Vec<f32> {
    let plane = image.width() as usize * image.height() as usize;
    let mut data = vec![0.; plane * 3];
    let mean = [0.485, 0.456, 0.406];
    let std = [0.229, 0.224, 0.225];
    for (i, p) in image.pixels().enumerate() {
        for c in 0..3 {
            let x = f32::from(p[c]) / 255.;
            data[c * plane + i] = if detection {
                (x - mean[c]) / std[c]
            } else {
                (x - 0.5) / 0.5
            };
        }
    }
    data
}
pub fn crop(image: &RgbImage, q: Quad) -> Result<(RgbImage, Quad)> {
    let width = distance(q[0], q[1])
        .max(distance(q[3], q[2]))
        .round()
        .clamp(2., 4096.) as u32;
    let height = distance(q[0], q[3])
        .max(distance(q[1], q[2]))
        .round()
        .clamp(2., 4096.) as u32;
    let mut crop = RgbImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let p = project(
                q,
                f64::from(x) / f64::from(width - 1),
                f64::from(y) / f64::from(height - 1),
            )
            .context("degenerate crop quad")?;
            let sx = p[0].clamp(0., f64::from(image.width() - 1));
            let sy = p[1].clamp(0., f64::from(image.height() - 1));
            let x0 = sx.floor() as u32;
            let y0 = sy.floor() as u32;
            let x1 = (x0 + 1).min(image.width() - 1);
            let y1 = (y0 + 1).min(image.height() - 1);
            let fx = sx - f64::from(x0);
            let fy = sy - f64::from(y0);
            let pixel = std::array::from_fn(|c| {
                ((f64::from(image.get_pixel(x0, y0)[c]) * (1. - fx)
                    + f64::from(image.get_pixel(x1, y0)[c]) * fx)
                    * (1. - fy)
                    + (f64::from(image.get_pixel(x0, y1)[c]) * (1. - fx)
                        + f64::from(image.get_pixel(x1, y1)[c]) * fx)
                        * fy)
                    .round() as u8
            });
            crop.put_pixel(x, y, Rgb(pixel));
        }
    }
    if f64::from(height) >= f64::from(width) * 1.5 {
        Ok((imageops::rotate270(&crop), [q[1], q[2], q[3], q[0]]))
    } else {
        Ok((crop, q))
    }
}
pub fn batch(
    images: &[RgbImage],
    height: u32,
    fixed_width: Option<u32>,
) -> (Vec<i64>, Vec<f32>, Vec<u32>) {
    let mut widths: Vec<u32> = images
        .iter()
        .map(|image| {
            (f64::from(image.width()) * f64::from(height) / f64::from(image.height()))
                .ceil()
                .clamp(1., 4096.) as u32
        })
        .collect();
    let width =
        fixed_width.unwrap_or_else(|| widths.iter().copied().max().unwrap_or(1).div_ceil(8) * 8);
    let item = 3 * height as usize * width as usize;
    let mut data = vec![0.; images.len() * item];
    for (i, image) in images.iter().enumerate() {
        widths[i] = widths[i].min(width);
        let w = widths[i];
        let resized = imageops::resize(image, w, height, FilterType::Triangle);
        let src = tensor(&resized, false);
        for c in 0..3 {
            for y in 0..height as usize {
                let dest = i * item + c * height as usize * width as usize + y * width as usize;
                let start = c * height as usize * w as usize + y * w as usize;
                data[dest..dest + w as usize].copy_from_slice(&src[start..start + w as usize]);
            }
        }
    }
    (
        vec![
            i64::try_from(images.len()).expect("batch size fits in i64"),
            3,
            i64::from(height),
            i64::from(width),
        ],
        data,
        widths,
    )
}
