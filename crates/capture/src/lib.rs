//! Screen capture behind the [`ScreenCapture`] trait.
//!
//! Implementations: live capture of the active display lives in the
//! platform crate (`glance-macos`, ScreenCaptureKit); [`PngFile`] reads a PNG
//! supplied on the command line for the CLI spikes. This crate holds no
//! platform code.
//!
//! Screenshots are held in opaque in-memory types: no `Serialize`, no
//! `Display`, and a `Debug` that prints only dimensions and sizes, so pixels
//! cannot reach a log or a file by accident. See `docs/DESIGN.md` §2–4.

use std::fmt;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not decode the image as PNG: {0}")]
    Decode(#[source] image::ImageError),
    #[error("could not encode the image as PNG: {0}")]
    Encode(#[source] image::ImageError),
    #[error(
        "Screen Recording permission is not granted. Allow glance in System Settings > \
         Privacy & Security > Screen & System Audio Recording, then relaunch it"
    )]
    PermissionDenied,
    #[error("no display to capture")]
    NoDisplay,
    #[error("screen capture failed: {0}")]
    Capture(String),
}

/// A source of screenshots.
pub trait ScreenCapture: Send + Sync {
    /// Capture the active display.
    fn capture(&self) -> Result<Screenshot, CaptureError>;
}

/// A decoded screenshot, in memory only.
pub struct Screenshot {
    image: DynamicImage,
}

impl fmt::Debug for Screenshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Screenshot({}x{})", self.width(), self.height())
    }
}

impl Screenshot {
    pub fn from_png(bytes: &[u8]) -> Result<Self, CaptureError> {
        let image = image::load_from_memory_with_format(bytes, ImageFormat::Png)
            .map_err(CaptureError::Decode)?;
        Ok(Self { image })
    }

    /// Wrap tightly packed RGBA pixels (4 bytes per pixel, no row padding).
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, CaptureError> {
        let image = image::RgbaImage::from_raw(width, height, pixels).ok_or_else(|| {
            CaptureError::Capture(format!("pixel buffer does not match {width}x{height}"))
        })?;
        Ok(Self {
            image: DynamicImage::ImageRgba8(image),
        })
    }

    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// Downscale so the longer edge is at most `max_long_edge` (never
    /// upscale), drop alpha, and encode as PNG with fast compression.
    pub fn encode(&self, max_long_edge: u32) -> Result<EncodedImage, CaptureError> {
        let (w, h) = fit_within(self.width(), self.height(), max_long_edge);
        let rgb = if (w, h) == (self.width(), self.height()) {
            self.image.to_rgb8()
        } else {
            // CatmullRom keeps small text legible when shrinking by 2x or more.
            self.image
                .resize_exact(w, h, FilterType::CatmullRom)
                .to_rgb8()
        };
        let mut png = Vec::new();
        let encoder = PngEncoder::new_with_quality(
            Cursor::new(&mut png),
            CompressionType::Fast,
            PngFilter::Adaptive,
        );
        DynamicImage::ImageRgb8(rgb)
            .write_with_encoder(encoder)
            .map_err(CaptureError::Encode)?;
        Ok(EncodedImage {
            png: Arc::from(png),
            width: w,
            height: h,
        })
    }
}

/// PNG bytes ready to send to a model, in memory only.
#[derive(Clone)]
pub struct EncodedImage {
    png: Arc<[u8]>,
    width: u32,
    height: u32,
}

impl fmt::Debug for EncodedImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "EncodedImage({}x{}, {} kB)",
            self.width,
            self.height,
            self.png.len() / 1000
        )
    }
}

impl EncodedImage {
    pub fn png(&self) -> Arc<[u8]> {
        Arc::clone(&self.png)
    }

    pub fn len(&self) -> usize {
        self.png.len()
    }

    pub fn is_empty(&self) -> bool {
        self.png.is_empty()
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

/// Reads a PNG from disk. Used by the CLI spikes before live capture exists.
#[derive(Debug, Clone)]
pub struct PngFile {
    path: PathBuf,
}

impl PngFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl ScreenCapture for PngFile {
    fn capture(&self) -> Result<Screenshot, CaptureError> {
        let bytes = std::fs::read(&self.path).map_err(|source| CaptureError::Io {
            path: self.path.clone(),
            source,
        })?;
        Screenshot::from_png(&bytes)
    }
}

/// Largest size with the same aspect ratio whose longer edge is `max` or less.
pub fn fit_within(width: u32, height: u32, max: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= max || long == 0 {
        return (width, height);
    }
    let scale = |edge: u32| {
        ((u64::from(edge) * u64::from(max) + u64::from(long) / 2) / u64::from(long)).max(1) as u32
    };
    (scale(width), scale(height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 7, 255])
        });
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn fit_within_keeps_aspect_ratio_and_never_upscales() {
        assert_eq!(fit_within(3024, 1964, 1280), (1280, 831));
        assert_eq!(fit_within(1964, 3024, 1280), (831, 1280));
        assert_eq!(fit_within(800, 600, 1280), (800, 600));
        assert_eq!(fit_within(4000, 1, 1000), (1000, 1));
    }

    #[test]
    fn encode_downscales_and_round_trips() {
        let shot = Screenshot::from_png(&png(640, 400)).unwrap();
        let small = shot.encode(320).unwrap();
        assert_eq!((small.width(), small.height()), (320, 200));
        let back = Screenshot::from_png(&small.png()).unwrap();
        assert_eq!((back.width(), back.height()), (320, 200));

        let same = shot.encode(4096).unwrap();
        assert_eq!((same.width(), same.height()), (640, 400));
    }

    #[test]
    fn debug_shows_dimensions_only() {
        let shot = Screenshot::from_png(&png(64, 32)).unwrap();
        assert_eq!(format!("{shot:?}"), "Screenshot(64x32)");
        let enc = shot.encode(64).unwrap();
        assert!(format!("{enc:?}").starts_with("EncodedImage(64x32, "));
    }

    #[test]
    fn from_rgba_checks_the_buffer_size() {
        let shot = Screenshot::from_rgba(4, 2, vec![200; 4 * 2 * 4]).unwrap();
        assert_eq!((shot.width(), shot.height()), (4, 2));
        assert!(Screenshot::from_rgba(4, 2, vec![0; 10]).is_err());
    }

    #[test]
    fn rejects_non_png_input() {
        assert!(matches!(
            Screenshot::from_png(b"definitely not a png"),
            Err(CaptureError::Decode(_))
        ));
    }
}
