//! Diagnóstico de captura: tiempo, tamaño y cuánto sale negro en el monitor bajo el cursor.
//!
//!   cargo run --example capture_check [-- salida.png]

use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let monitors = xcap::Monitor::all()?;
    let m = monitors.iter().find(|m| m.is_primary().unwrap_or(false)).unwrap_or(&monitors[0]);
    let (x, y) = (m.x()? + 10, m.y()? + 10);
    for i in 0..3 {
        let t0 = Instant::now();
        let img = lector_lib::capture::grab_monitor_at(x, y)?;
        let t_cap = t0.elapsed().as_millis();
        let t1 = Instant::now();
        let jpeg = lector_lib::capture::to_jpeg(&img)?;
        println!("  captura {t_cap} ms · JPEG {} KB en {} ms", jpeg.len() / 1024, t1.elapsed().as_millis());
        let dark = img.pixels().filter(|p| p.0[0] < 16 && p.0[1] < 16 && p.0[2] < 16).count();
        println!(
            "captura {i}: {}x{} en {} ms · {:.1} % negro · vacía: {}",
            img.width(),
            img.height(),
            t0.elapsed().as_millis(),
            100.0 * dark as f64 / (img.width() * img.height()) as f64,
            lector_lib::capture::is_blank(&img)
        );
        if i == 0 {
            if let Some(path) = std::env::args().nth(1) {
                img.save(&path)?;
            }
        }
    }
    Ok(())
}
