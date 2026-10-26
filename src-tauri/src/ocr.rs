//! OCR de Windows (Windows.Media.Ocr) como respaldo cuando LM Studio no responde.
//! No entiende fórmulas, pero permite leer texto normal sin el LLM.

use anyhow::{bail, Result};
use image::imageops::{self, FilterType};
use image::RgbaImage;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;

pub async fn recognize(img: RgbaImage) -> Result<String> {
    crate::winrt::run(move || recognize_blocking(img)).await?
}

fn recognize_blocking(mut img: RgbaImage) -> Result<String> {
    let max = OcrEngine::MaxImageDimension()?;
    if img.width() > max || img.height() > max {
        let s = f64::from(max) / f64::from(img.width().max(img.height()));
        let (w, h) = ((f64::from(img.width()) * s) as u32, (f64::from(img.height()) * s) as u32);
        img = imageops::resize(&img, w, h, FilterType::Triangle);
    }
    let (w, h) = img.dimensions();
    let mut bgra = img.into_raw();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buffer = CryptographicBuffer::CreateFromByteArray(&bgra)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32)?;
    let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() else {
        bail!("no hay idiomas de OCR instalados en Windows");
    };
    let result = engine.RecognizeAsync(&bitmap)?.join()?;
    let lines = result.Lines()?;
    let mut out = String::new();
    for i in 0..lines.Size()? {
        out.push_str(&lines.GetAt(i)?.Text()?.to_string());
        out.push('\n');
    }
    Ok(out)
}
