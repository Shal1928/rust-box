//! Image loading and colour manipulation utilities.
//!
//! This module provides [`IcoMgr`], which loads a PNG image, stores its raw RGBA data,
//! and offers methods to create coloured versions of the image data. It does not depend
//! on any specific tray or GUI library; it only works with raw pixel arrays.

use std::io::Cursor;
use image::{ImageReader, Rgba, RgbaImage};

/// Manages raw RGBA image data and provides colour transformation methods.
///
/// The manager loads an image from bytes or a file path, decodes it as RGBA,
/// and keeps the pixel data in memory. You can obtain the original pixel data
/// or apply recolouring (e.g., turning white pixels into green or blue) and
/// get the resulting `RgbaImage` or raw RGBA bytes.
///
/// # Examples
///
/// ```no_run
/// use rust_box::icons::IcoMgr;
///
/// let mgr = IcoMgr::new("assets/icon.png")?;
/// let original_data = mgr.raw_rgba();
/// let green_data = mgr.green().into_raw();
/// // Later, create a tray icon from the data:
/// // let icon = tray_icon::Icon::from_rgba(green_data, mgr.width(), mgr.height())?;
/// ```
pub struct IcoMgr {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

impl IcoMgr {
    /// Creates a new `IcoMgr` from an image file path.
    ///
    /// # Errors
    /// Returns `image::ImageError` if the file cannot be read or decoded.
    pub fn from_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self, image::ImageError> {
        let img = ImageReader::open(path)?.decode()?.into_rgba8();
        let (width, height) = img.dimensions();
        Ok(Self {
            rgba: img.into_raw(),
            width,
            height,
        })
    }

    /// Creates a new `IcoMgr` from a byte slice (e.g., from `include_bytes!`).
    ///
    /// # Errors
    /// Returns `image::ImageError` if the data cannot be decoded or is not a valid PNG.
    pub fn from_bytes(data: &[u8]) -> Result<Self, image::ImageError> {
        let img = ImageReader::new(Cursor::new(data))
            .with_guessed_format()?
            .decode()?
            .into_rgba8();
        let (width, height) = img.dimensions();
        Ok(Self {
            rgba: img.into_raw(),
            width,
            height,
        })
    }

    /// Convenience alias for `from_path`.
    ///
    /// # Errors
    /// Same as [`from_path`](Self::from_path).
    pub fn new<P: AsRef<std::path::Path>>(path: P) -> Result<Self, image::ImageError> {
        Self::from_path(path)
    }

    /// Returns the raw RGBA pixel data.
    pub fn raw_rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Returns the image width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the image height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Creates a recoloured version of the image as an `RgbaImage`.
    ///
    /// Only white pixels (R>200, G>200, B>200) are recoloured to the given colour.
    /// Transparent pixels are left unchanged.
    ///
    /// # Arguments
    /// * `color` – `[r, g, b, a]` colour to apply.
    pub fn colored_image(&self, color: [u8; 4]) -> RgbaImage {
        let mut img = RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .expect("Failed to recreate RgbaImage");
        let target = Rgba(color);

        for y in 0..self.height {
            for x in 0..self.width {
                let pixel = img.get_pixel_mut(x, y);
                let [r, g, b, a] = pixel.0;
                if a == 0 {
                    continue;
                }
                if r > 200 && g > 200 && b > 200 {
                    *pixel = target;
                }
            }
        }
        img
    }

    /// Returns a green‑tinted version of the image as raw RGBA bytes.
    pub fn green_raw(&self) -> Vec<u8> {
        self.colored_image([0, 255, 0, 255]).into_raw()
    }

    /// Returns a blue‑tinted version of the image as raw RGBA bytes.
    pub fn blue_raw(&self) -> Vec<u8> {
        self.colored_image([66, 170, 255, 255]).into_raw()  // #42AAFF
    }

    /// Returns a recoloured version of the image as raw RGBA bytes.
    pub fn colored_raw(&self, color: [u8; 4]) -> Vec<u8> {
        self.colored_image(color).into_raw()
    }
}