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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use image::{DynamicImage, ImageFormat};
    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Encodes the given pixels into an in-memory PNG.
    /// `pixels` must be laid out row-major, `width * height` entries.
    fn make_test_png(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        assert_eq!(
            pixels.len(),
            (width * height) as usize,
            "pixel count must match dimensions"
        );
        let mut img = RgbaImage::new(width, height);
        for (i, p) in pixels.iter().enumerate() {
            let x = (i as u32) % width;
            let y = (i as u32) / width;
            img.put_pixel(x, y, Rgba(*p));
        }
        let mut buf = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .expect("Failed to encode test PNG");
        buf
    }

    /// Creates a unique path in the temp directory (file is NOT created).
    fn unique_temp_path(ext: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("icore_test_{pid}_{n}.{ext}"))
    }

    #[test]
    fn from_bytes_valid_png_ok() {
        let png = make_test_png(2, 2, &[[255, 255, 255, 255]; 4]);
        let mgr = IcoMgr::from_bytes(&png).expect("valid PNG should load");
        assert_eq!(mgr.width(), 2);
        assert_eq!(mgr.height(), 2);
        assert_eq!(mgr.raw_rgba().len(), 2 * 2 * 4);
    }

    #[test]
    fn from_bytes_fully_transparent_png_ok() {
        let png = make_test_png(1, 1, &[[0, 0, 0, 0]]);
        let mgr = IcoMgr::from_bytes(&png).expect("transparent PNG is valid");
        assert_eq!(mgr.raw_rgba(), &[0, 0, 0, 0]);
    }

    #[test]
    fn from_bytes_invalid_data_err() {
        assert!(IcoMgr::from_bytes(b"not a png at all").is_err());
    }

    #[test]
    fn from_bytes_empty_err() {
        assert!(IcoMgr::from_bytes(&[]).is_err());
    }

    #[test]
    fn from_bytes_truncated_png_err() {
        let png = make_test_png(2, 2, &[[255, 255, 255, 255]; 4]);
        let truncated = &png[..png.len() / 2];
        assert!(IcoMgr::from_bytes(truncated).is_err());
    }

    #[test]
    fn from_path_valid_file_ok() {
        let png = make_test_png(1, 1, &[[255, 0, 0, 255]]);
        let path = unique_temp_path("png");
        std::fs::write(&path, &png).unwrap();

        let mgr = IcoMgr::from_path(&path).expect("valid file should load");
        assert_eq!(mgr.width(), 1);
        assert_eq!(mgr.height(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_path_missing_file_err() {
        let path = unique_temp_path("png");
        // without make_test_png and after fs::write
        assert!(IcoMgr::from_path(&path).is_err());
    }

    #[test]
    fn from_path_invalid_content_err() {
        let path = unique_temp_path("png");
        std::fs::write(&path, b"garbage bytes").unwrap();
        assert!(IcoMgr::from_path(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn new_is_alias_for_from_path() {
        let png = make_test_png(1, 1, &[[255, 255, 255, 255]]);
        let path = unique_temp_path("png");
        std::fs::write(&path, &png).unwrap();
        assert!(IcoMgr::new(&path).is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn raw_rgba_preserves_pixels() {
        let pixels = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [0, 0, 0, 0],
        ];
        let png = make_test_png(2, 2, &pixels);
        let mgr = IcoMgr::from_bytes(&png).unwrap();

        let expected: Vec<u8> = pixels.iter().flatten().copied().collect();
        assert_eq!(mgr.raw_rgba(), expected.as_slice());
    }

    #[test]
    fn raw_rgba_len_is_w_times_h_times_4() {
        let w = 3;
        let h = 5;
        let rgba = [100, 100, 100, 255];

        let png = make_test_png(w, h, &[rgba; 15]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        assert_eq!(mgr.raw_rgba().len() as u32, w * h * rgba.len() as u32);
    }

    #[test]
    fn colored_image_recolors_white() {
        let png = make_test_png(1, 1, &[[255, 255, 255, 255]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        let img = mgr.colored_image([10, 20, 30, 255]);
        assert_eq!(img.get_pixel(0, 0).0, [10, 20, 30, 255]);
    }

    #[test]
    fn colored_image_recolors_near_white() {
        // Threshold: > 200 for each channel.
        let png = make_test_png(1, 1, &[[201, 210, 220, 255]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        let img = mgr.colored_image([0, 0, 0, 255]);
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 0, 255]);
    }

    #[test]
    fn colored_image_preserves_non_white() {
        // 200 on a channel is NOT > 200 → должен остаться.
        let png = make_test_png(1, 1, &[[200, 150, 100, 255]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        let img = mgr.colored_image([0, 255, 0, 255]);
        assert_eq!(img.get_pixel(0, 0).0, [200, 150, 100, 255]);
    }

    #[test]
    fn colored_image_does_not_touch_transparent() {
        // Полностью прозрачный белый — должен остаться как есть.
        let png = make_test_png(1, 1, &[[255, 255, 255, 0]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        let img = mgr.colored_image([0, 255, 0, 255]);
        assert_eq!(img.get_pixel(0, 0).0, [255, 255, 255, 0]);
    }

    #[test]
    fn colored_image_replaces_alpha_for_opaque_white() {
        // Текущее поведение: для непрозрачного белого заменяется весь пиксель,
        // включая альфу, на target. Альфа оригинала НЕ сохраняется.
        let png = make_test_png(1, 1, &[[255, 255, 255, 128]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        let img = mgr.colored_image([0, 0, 0, 200]);
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 0, 200]);
    }

    #[test]
    fn colored_image_preserves_dimensions() {
        let png = make_test_png(4, 3, &[[255, 255, 255, 255]; 12]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        let img = mgr.colored_image([0, 0, 0, 255]);
        assert_eq!(img.dimensions(), (4, 3));
    }

    #[test]
    fn colored_image_mixed_pixels() {
        let pixels = [
            [255, 255, 255, 255], // white   → recolor
            [255, 0, 0, 255],     // red     → keep
            [255, 255, 255, 0],   // transp. → keep
            [50, 50, 50, 255],    // dark    → keep
        ];
        let png = make_test_png(4, 1, &pixels);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        let img = mgr.colored_image([1, 2, 3, 255]);

        assert_eq!(img.get_pixel(0, 0).0, [1, 2, 3, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(2, 0).0, [255, 255, 255, 0]);
        assert_eq!(img.get_pixel(3, 0).0, [50, 50, 50, 255]);
    }

    #[test]
    fn colored_image_does_not_mutate_internal_state() {
        let png = make_test_png(1, 1, &[[255, 255, 255, 255]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();

        let _ = mgr.colored_image([0, 0, 0, 255]);
        let _ = mgr.colored_image([0, 255, 0, 255]);

        // Внутренний RGBA должен остаться белым.
        assert_eq!(mgr.raw_rgba(), &[255, 255, 255, 255]);
    }

    #[test]
    fn green_raw_produces_opaque_green() {
        let png = make_test_png(1, 1, &[[255, 255, 255, 255]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        assert_eq!(mgr.green_raw(), vec![0, 255, 0, 255]);
    }

    #[test]
    fn blue_raw_produces_expected_blue() {
        let png = make_test_png(1, 1, &[[255, 255, 255, 255]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        assert_eq!(mgr.blue_raw(), vec![0x42, 0xAA, 0xFF, 255]);
    }

    #[test]
    fn colored_raw_custom_color() {
        let png = make_test_png(1, 1, &[[255, 255, 255, 255]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        assert_eq!(mgr.colored_raw([7, 8, 9, 255]), vec![7, 8, 9, 255]);
    }

    #[test]
    fn green_raw_len_matches_dimensions() {
        let png = make_test_png(2, 3, &[[255, 255, 255, 255]; 6]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        assert_eq!(mgr.green_raw().len(), (2 * 3 * 4) as usize);
    }

    #[test]
    fn green_and_blue_differ_only_in_color_channels() {
        let png = make_test_png(1, 1, &[[255, 255, 255, 255]]);
        let mgr = IcoMgr::from_bytes(&png).unwrap();
        let g = mgr.green_raw();
        let b = mgr.blue_raw();
        assert_eq!(g.len(), b.len());
        assert_ne!(g, b);

        assert_eq!(g[3], b[3]);
    }

    #[test]
    fn embedded_project_icon_loads_and_has_nonzero_size() {
        let bytes = include_bytes!("../rust-box.png");
        let mgr = IcoMgr::from_bytes(bytes).expect("project icon must decode");
        assert!(mgr.width() > 0);
        assert!(mgr.height() > 0);
        assert_eq!(
            mgr.raw_rgba().len() as u32,
            mgr.width() * mgr.height() * 4
        );

        let _ = mgr.green_raw();
        let _ = mgr.blue_raw();
    }
}