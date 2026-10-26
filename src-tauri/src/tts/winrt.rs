//! Voces del sistema en Windows (Windows.Media.SpeechSynthesis) con marcas por palabra.

use std::cell::RefCell;

use anyhow::{Context, Result};
use windows::core::{Interface, HSTRING};
use windows::Media::Core::SpeechCue;
use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
use windows::Storage::Streams::DataReader;

use super::{Mark, Utterance, VoiceInfo};
use crate::doc::script::utf16_to_byte;
use crate::lang::Lang;

thread_local! {
    // Vive en el hilo WinRT (ver `crate::winrt`), así que se crea una sola vez.
    static SYNTH: RefCell<Option<SpeechSynthesizer>> = const { RefCell::new(None) };
}

pub async fn synthesize(voice: String, lang: Lang, rate: f32, text: String) -> Result<Utterance> {
    crate::winrt::run(move || synth_blocking(&voice, lang, rate, &text)).await?
}

pub async fn voices() -> Result<Vec<VoiceInfo>> {
    crate::winrt::run(|| -> Result<Vec<VoiceInfo>> {
        let all = SpeechSynthesizer::AllVoices()?;
        let mut out = Vec::new();
        for i in 0..all.Size()? {
            let v = all.GetAt(i)?;
            let name = v.DisplayName()?.to_string();
            out.push(VoiceInfo {
                id: name.clone(),
                name,
                locale: v.Language()?.to_string(),
            });
        }
        Ok(out)
    })
    .await?
}

fn synthesizer() -> windows::core::Result<SpeechSynthesizer> {
    SYNTH.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let s = SpeechSynthesizer::new()?;
        s.Options()?.SetIncludeWordBoundaryMetadata(true)?;
        *slot = Some(s.clone());
        Ok(s)
    })
}

/// La voz con ese nombre o, si no existe, la primera del idioma pedido.
fn find_voice(name: &str, lang: Lang) -> windows::core::Result<Option<VoiceInformation>> {
    let all = SpeechSynthesizer::AllVoices()?;
    let mut by_lang = None;
    for i in 0..all.Size()? {
        let v = all.GetAt(i)?;
        if v.DisplayName()?.to_string().eq_ignore_ascii_case(name) {
            return Ok(Some(v));
        }
        if by_lang.is_none() && v.Language()?.to_string().to_lowercase().starts_with(lang.code()) {
            by_lang = Some(v);
        }
    }
    Ok(by_lang)
}

fn synth_blocking(voice: &str, lang: Lang, rate: f32, text: &str) -> Result<Utterance> {
    let synth = synthesizer()?;
    if let Some(v) = find_voice(voice, lang)? {
        if synth.Voice()?.Id()? != v.Id()? {
            synth.SetVoice(&v)?;
        }
    }
    synth.Options()?.SetSpeakingRate(f64::from(rate.clamp(0.5, 3.0)))?;
    let stream = synth.SynthesizeTextToStreamAsync(&HSTRING::from(text))?.join()?;

    let mut marks = Vec::new();
    let tracks = stream.TimedMetadataTracks()?;
    let n_tracks = tracks.Size()?;
    for i in 0..n_tracks {
        let track = tracks.GetAt(i)?;
        if n_tracks > 1 && !track.Id()?.to_string().to_lowercase().contains("word") {
            continue;
        }
        let cues = track.Cues()?;
        for j in 0..cues.Size()? {
            let cue: SpeechCue = cues.GetAt(j)?.cast()?;
            let Ok(pos) = cue.StartPositionInInput().and_then(|p| p.Value()) else {
                continue;
            };
            marks.push(Mark {
                t_ms: (cue.StartTime()?.Duration / 10_000) as u32,
                dur_ms: (cue.Duration()?.Duration / 10_000) as u32,
                byte: utf16_to_byte(text, pos.max(0) as usize),
            });
        }
    }
    marks.sort_by_key(|m| m.t_ms);

    let size = u32::try_from(stream.Size()?).context("audio demasiado grande")?;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.LoadAsync(size)?.join()?;
    let mut audio = vec![0u8; size as usize];
    reader.ReadBytes(&mut audio)?;
    // WinRT no informa la duración de las palabras: el fin de la voz se mide en el audio.
    let end_ms = super::wav::speech_end_ms(&audio);
    Ok(Utterance { audio, marks, end_ms })
}
