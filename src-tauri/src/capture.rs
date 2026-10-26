//! Captura del monitor, recorte de la selección y preparación para el modelo.

use anyhow::{Context, Result};
use base64::Engine as _;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::{self, FilterType};
use image::{ExtendedColorType, ImageEncoder, RgbImage, RgbaImage};
use serde::Deserialize;

/// Lado mayor máximo que se manda al modelo. La API cobra la imagen por tamaño y no la
/// reduce sola; a 1568 px se leyó igual de bien que a resolución completa (medido).
const MAX_SIDE: u32 = 1568;
/// Por debajo de esta altura se amplía ×2 (texto de una sola línea pequeña).
const MIN_HEIGHT: u32 = 40;

/// Rectángulo en fracciones (0..1) del monitor capturado; no depende del DPI.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct FracRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Captura completa del monitor que contiene el punto (coordenadas físicas).
pub fn grab_monitor_at(x: i32, y: i32) -> Result<RgbaImage> {
    let monitor = xcap::Monitor::from_point(x, y).context("no se encontró el monitor bajo el cursor")?;
    monitor.capture_image().context("no se pudo capturar la pantalla")
}

/// JPEG de la captura completa, para mostrarla congelada mientras se selecciona.
pub fn to_jpeg(img: &RgbaImage) -> Result<Vec<u8>> {
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let mut out = Vec::with_capacity(rgb.len() / 8);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88).write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

/// La imagen es casi toda negra: típico de un video protegido contra capturas (DRM).
pub fn is_blank(img: &RgbaImage) -> bool {
    let total = u64::from(img.width()) * u64::from(img.height());
    if total == 0 {
        return true;
    }
    let dark = img.pixels().filter(|p| p.0[0] < 16 && p.0[1] < 16 && p.0[2] < 16).count() as u64;
    dark * 100 >= total * 98
}

pub fn crop(img: &RgbaImage, r: FracRect) -> Option<RgbaImage> {
    let (w, h) = (f64::from(img.width()), f64::from(img.height()));
    // `round` y no floor/ceil: las fracciones traen error de coma flotante.
    let x0 = (r.x.clamp(0.0, 1.0) * w).round() as u32;
    let y0 = (r.y.clamp(0.0, 1.0) * h).round() as u32;
    let x1 = ((r.x + r.w).clamp(0.0, 1.0) * w).round() as u32;
    let y1 = ((r.y + r.h).clamp(0.0, 1.0) * h).round() as u32;
    if x1 <= x0 + 4 || y1 <= y0 + 4 {
        return None;
    }
    Some(imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image())
}

/// Ajusta el tamaño para el modelo de visión y lo codifica como PNG en base64.
pub fn to_png_base64(img: &RgbaImage) -> Result<String> {
    let mut rgb = RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let p = img.get_pixel(x, y).0;
        image::Rgb([p[0], p[1], p[2]])
    });
    let (w, h) = rgb.dimensions();
    let longest = w.max(h);
    if longest > MAX_SIDE {
        let s = f64::from(MAX_SIDE) / f64::from(longest);
        let (nw, nh) = ((f64::from(w) * s) as u32, (f64::from(h) * s) as u32);
        rgb = imageops::resize(&rgb, nw.max(1), nh.max(1), FilterType::Triangle);
    } else if h < MIN_HEIGHT {
        rgb = imageops::resize(&rgb, w * 2, h * 2, FilterType::CatmullRom);
    }
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, PngFilter::Adaptive).write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorte_por_fracciones() {
        let img = RgbaImage::new(1000, 500);
        let c = crop(&img, FracRect { x: 0.1, y: 0.2, w: 0.5, h: 0.4 }).unwrap();
        assert_eq!(c.dimensions(), (500, 200));
        assert!(crop(&img, FracRect { x: 0.5, y: 0.5, w: 0.001, h: 0.3 }).is_none());
    }

    #[test]
    fn detecta_capturas_negras() {
        assert!(is_blank(&RgbaImage::new(200, 100)));
        // Texto blanco sobre negro (una diapositiva oscura) no cuenta como vacía.
        let mut slide = RgbaImage::new(200, 100);
        for x in 20..180 {
            for y in 40..50 {
                slide.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        assert!(!is_blank(&slide));
    }
}
