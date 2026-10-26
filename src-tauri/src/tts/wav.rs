//! Análisis mínimo de WAV PCM de 16 bits (lo que entregan WinRT y `say`).

/// Muestras por debajo de esto se consideran silencio.
const THRESHOLD: u16 = 400;

/// Primer y último milisegundo con sonido.
pub fn speech_bounds_ms(wav: &[u8]) -> Option<(u32, u32)> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |i: usize| Some(u16::from_le_bytes(wav.get(i..i + 2)?.try_into().ok()?));
    let u32_at = |i: usize| Some(u32::from_le_bytes(wav.get(i..i + 4)?.try_into().ok()?));
    let (mut channels, mut rate, mut bits) = (0usize, 0u64, 0u16);
    let mut pos = 12;
    while pos + 8 <= wav.len() {
        let id = &wav[pos..pos + 4];
        let size = u32_at(pos + 4)? as usize;
        let body = pos + 8;
        if id == b"fmt " {
            channels = usize::from(u16_at(body + 2)?);
            rate = u64::from(u32_at(body + 4)?);
            bits = u16_at(body + 14)?;
        } else if id == b"data" {
            if bits != 16 || channels == 0 || rate == 0 {
                return None;
            }
            let data = &wav[body..(body + size).min(wav.len())];
            let loud = |f: &[u8]| f.chunks_exact(2).any(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs() > THRESHOLD);
            let mut frames = data.chunks_exact(2 * channels);
            let first = frames.clone().position(loud)?;
            let last = frames.rposition(loud)?;
            let ms = |frame: usize| (frame as u64 * 1000 / rate) as u32;
            return Some((ms(first), ms(last + 1)));
        }
        pos = body + size + (size & 1);
    }
    None
}

/// Último milisegundo con sonido (lo que sigue es silencio que se puede cortar).
pub fn speech_end_ms(wav: &[u8]) -> Option<u32> {
    speech_bounds_ms(wav).map(|(_, end)| end)
}

#[cfg(test)]
pub(crate) fn build(samples: &[i16], rate: u32) -> Vec<u8> {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut w = Vec::new();
    w.extend(b"RIFF");
    w.extend((36 + data.len() as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes()); // PCM
    w.extend(1u16.to_le_bytes()); // mono
    w.extend(rate.to_le_bytes());
    w.extend((rate * 2).to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((data.len() as u32).to_le_bytes());
    w.extend(data);
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limites_de_la_voz() {
        // 1000 Hz: 100 ms de silencio, 300 con sonido y 600 de silencio.
        let mut s = vec![0i16; 100];
        s.extend(vec![3000i16; 300]);
        s.extend(vec![0i16; 600]);
        assert_eq!(speech_bounds_ms(&build(&s, 1000)), Some((100, 400)));
        assert_eq!(speech_end_ms(&build(&s, 1000)), Some(400));
        assert_eq!(speech_end_ms(&build(&[0; 50], 1000)), None);
        assert_eq!(speech_end_ms(b"no es un wav"), None);
    }
}
