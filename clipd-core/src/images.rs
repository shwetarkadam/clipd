//! On-disk storage for image clips.
//!
//! Clipboard images (screenshots, copied graphics) are saved as PNGs under
//! `<data_local>/clipd/images/`, alongside a small thumbnail used to render the
//! history list cheaply. Files are content-addressed by a hash of the raw
//! pixels, so copying the same image twice reuses one file on disk.

use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// Longest edge (px) of the generated thumbnail. Big enough to be recognizable
/// in the list, small enough to decode instantly.
const THUMB_MAX_EDGE: u32 = 360;

/// Result of persisting a clipboard image to disk.
#[derive(Debug, Clone)]
pub struct SavedImage {
    pub full_path: PathBuf,
    pub thumb_path: PathBuf,
    pub hash: String,
    pub width: u32,
    pub height: u32,
}

/// Directory where image clips live: `<data_local>/clipd/images/`.
pub fn images_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("images")
}

/// Content hash of raw RGBA pixels (plus dimensions to avoid collisions between
/// differently-sized images that happen to share a byte prefix length).
pub fn hash_rgba(width: usize, height: usize, rgba: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((width as u64).to_le_bytes());
    hasher.update((height as u64).to_le_bytes());
    hasher.update(rgba);
    format!("{:x}", hasher.finalize())
}

/// Persist an RGBA8 image (as produced by `arboard::ImageData`) to disk as a
/// full PNG plus a thumbnail. Idempotent: if the same image was already saved,
/// the existing files are reused rather than re-encoded.
pub fn save_rgba_image(width: usize, height: usize, rgba: &[u8]) -> io::Result<SavedImage> {
    if width == 0 || height == 0 || rgba.len() < width * height * 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty or malformed image data",
        ));
    }

    let dir = images_dir();
    std::fs::create_dir_all(&dir)?;

    let hash = hash_rgba(width, height, rgba);
    let full_path = dir.join(format!("{}.png", hash));
    let thumb_path = dir.join(format!("{}_thumb.png", hash));

    let img = image::RgbaImage::from_raw(width as u32, height as u32, rgba.to_vec())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "image buffer size mismatch"))?;

    // Reuse existing files when re-copying the same image.
    if !full_path.exists() {
        img.save(&full_path)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    }
    if !thumb_path.exists() {
        // Scale the longest edge down to THUMB_MAX_EDGE, preserving aspect ratio.
        let (tw, th) = if width >= height {
            let tw = THUMB_MAX_EDGE.min(width as u32);
            let th = ((tw as f32) * (height as f32) / (width as f32)).round() as u32;
            (tw, th.max(1))
        } else {
            let th = THUMB_MAX_EDGE.min(height as u32);
            let tw = ((th as f32) * (width as f32) / (height as f32)).round() as u32;
            (tw.max(1), th)
        };
        let thumb = image::imageops::thumbnail(&img, tw, th);
        thumb
            .save(&thumb_path)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    }

    Ok(SavedImage {
        full_path,
        thumb_path,
        hash,
        width: width as u32,
        height: height as u32,
    })
}

/// Decode a PNG on disk back to (width, height, RGBA8) — used by the GUI to
/// upload a texture, and by the paste path to put an image on the clipboard.
pub fn load_rgba(path: &Path) -> io::Result<(u32, u32, Vec<u8>)> {
    let img = image::open(path)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Ok((w, h, img.into_raw()))
}

/// Remove the files backing an image clip (best-effort; ignores missing files).
pub fn delete_image_files(image_path: Option<&str>, thumb_path: Option<&str>) {
    for p in [image_path, thumb_path].into_iter().flatten() {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_writes_full_and_thumb_and_dedups() {
        // 8x4 solid RGBA image.
        let (w, h) = (8usize, 4usize);
        let rgba = vec![120u8; w * h * 4];
        let a = save_rgba_image(w, h, &rgba).expect("save");
        assert!(a.full_path.exists());
        assert!(a.thumb_path.exists());
        assert_eq!(a.width, 8);
        assert_eq!(a.height, 4);

        // Same pixels → same hash → same files (idempotent).
        let b = save_rgba_image(w, h, &rgba).expect("save again");
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.full_path, b.full_path);

        // Roundtrip decode.
        let (dw, dh, _) = load_rgba(&a.full_path).expect("decode");
        assert_eq!((dw, dh), (8, 4));

        delete_image_files(
            a.full_path.to_str(),
            a.thumb_path.to_str(),
        );
        assert!(!a.full_path.exists());
    }

    #[test]
    fn rejects_malformed() {
        assert!(save_rgba_image(0, 0, &[]).is_err());
        assert!(save_rgba_image(4, 4, &[0u8; 4]).is_err()); // too few bytes
    }
}
