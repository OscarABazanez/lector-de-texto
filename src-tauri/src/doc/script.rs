//! Fragmentos de lectura y guion hablado con su mapa hacia el documento.

use serde::Serialize;

use super::model::{SegKind, Segment};

/// Agrupa segmentos consecutivos en fragmentos de lectura (≈ una oración).
#[derive(Default)]
pub struct Chunker {
    current: Vec<usize>,
    words: usize,
}

const MAX_WORDS: usize = 45;

impl Chunker {
    /// Devuelve los fragmentos que quedan cerrados al añadir `seg`.
    pub fn push(&mut self, seg: &Segment) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        let standalone = seg.kind == SegKind::Heading || (seg.kind == SegKind::Math && seg.block);
        if (seg.para || standalone || self.words >= MAX_WORDS) && !self.current.is_empty() {
            out.push(self.take());
        }
        self.current.push(seg.id);
        self.words += seg.words.len().max(1);
        if standalone || seg.ends_sentence() {
            out.push(self.take());
        }
        out
    }

    pub fn finish(&mut self) -> Option<Vec<usize>> {
        (!self.current.is_empty()).then(|| self.take())
    }

    fn take(&mut self) -> Vec<usize> {
        self.words = 0;
        std::mem::take(&mut self.current)
    }
}

/// Qué resaltar: una palabra de un segmento de texto o una fórmula completa.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Target {
    pub seg: usize,
    pub word: Option<usize>,
}

#[derive(Debug)]
struct Piece {
    start: usize,
    target: Target,
}

/// Texto que se manda al motor de voz y el mapa byte → palabra/fórmula.
#[derive(Debug, Default)]
pub struct Script {
    pub text: String,
    pieces: Vec<Piece>,
}

impl Script {
    /// `spoken(seg)` devuelve la lectura de una fórmula en frases; en matemáticas
    /// `Target::word` es el número de frase (para resaltar el fragmento que lee).
    pub fn build<'a>(
        segs: impl IntoIterator<Item = &'a Segment>,
        spoken: impl Fn(usize) -> Option<Vec<String>>,
    ) -> Script {
        let mut s = Script::default();
        for seg in segs {
            match seg.kind {
                SegKind::Math => {
                    for (i, say) in spoken(seg.id).unwrap_or_default().iter().enumerate() {
                        let say = say.trim();
                        if !say.is_empty() {
                            s.push(say, Target { seg: seg.id, word: Some(i) });
                        }
                    }
                }
                SegKind::Text | SegKind::Heading => {
                    for (i, w) in seg.words.iter().enumerate() {
                        s.push(w, Target { seg: seg.id, word: Some(i) });
                    }
                }
            }
        }
        s
    }

    fn push(&mut self, piece: &str, target: Target) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.pieces.push(Piece { start: self.text.len(), target });
        self.text.push_str(piece);
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Número de piezas (palabras de texto y frases de fórmulas).
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    /// Posición de la pieza que resalta `target`.
    pub fn index_of(&self, target: Target) -> Option<usize> {
        self.pieces.iter().position(|p| p.target == target)
    }

    /// El guion desde la pieza `from` (para empezar a leer a mitad de un fragmento).
    pub fn tail(&self, from: usize) -> Script {
        let mut s = Script::default();
        for (i, p) in self.pieces.iter().enumerate().skip(from) {
            let end = self.pieces.get(i + 1).map_or(self.text.len(), |n| n.start - 1);
            s.push(&self.text[p.start..end], p.target);
        }
        s
    }

    pub fn first(&self) -> Option<Target> {
        self.pieces.first().map(|p| p.target)
    }

    /// Palabra o fórmula que contiene la posición `byte` del guion.
    pub fn locate(&self, byte: usize) -> Option<Target> {
        let idx = self.pieces.partition_point(|p| p.start <= byte);
        match idx {
            0 => self.first(),
            i => Some(self.pieces[i - 1].target),
        }
    }
}

/// Convierte una posición en unidades UTF-16 (como la reporta WinRT) a bytes UTF-8.
pub fn utf16_to_byte(text: &str, pos16: usize) -> usize {
    let mut units = 0;
    for (b, c) in text.char_indices() {
        if units >= pos16 {
            return b;
        }
        units += c.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parser::StreamParser;

    fn parse(s: &str) -> Vec<Segment> {
        let mut p = StreamParser::new();
        let mut v = p.push(s);
        v.extend(p.finish());
        v
    }

    #[test]
    fn fragmentos_por_oracion() {
        let segs = parse("Título\n\nUno dos tres. Cuatro $x$ cinco:\n\n$$y=1$$\nFin");
        let mut ch = Chunker::default();
        let mut chunks: Vec<Vec<usize>> = segs.iter().flat_map(|s| ch.push(s)).collect();
        chunks.extend(ch.finish());
        let texts: Vec<Vec<&str>> = chunks
            .iter()
            .map(|c| c.iter().map(|&i| segs[i].display.as_str()).collect())
            .collect();
        assert_eq!(
            texts,
            vec![
                vec!["Título"],
                vec!["Uno dos tres."],
                vec!["Cuatro", "x", "cinco:"],
                vec!["y=1"],
                vec!["Fin"],
            ]
        );
    }

    #[test]
    fn guion_y_mapa() {
        let segs = parse("entre los vectores $v_1$ y $v_2$ estaría");
        let script = Script::build(&segs, |id| {
            Some(match id {
                1 => vec!["ve sub uno".to_string()],
                _ => vec!["ve".to_string(), "sub dos".to_string()],
            })
        });
        assert_eq!(script.text, "entre los vectores ve sub uno y ve sub dos estaría");
        let pos = |w: &str| script.text.find(w).unwrap();
        assert_eq!(script.locate(pos("los")), Some(Target { seg: 0, word: Some(1) }));
        assert_eq!(script.locate(pos("sub uno")), Some(Target { seg: 1, word: Some(0) }));
        assert_eq!(script.locate(pos("sub dos")), Some(Target { seg: 3, word: Some(1) }));
        assert_eq!(script.locate(pos("y ve")), Some(Target { seg: 2, word: Some(0) }));
        assert_eq!(script.locate(pos("estaría")), Some(Target { seg: 4, word: Some(0) }));
    }

    #[test]
    fn guion_desde_una_pieza() {
        let segs = parse("entre los vectores $v_1$ y");
        let script = Script::build(&segs, |_| Some(vec!["ve sub".to_string(), "uno".to_string()]));
        assert_eq!(script.len(), 6);
        let t = script.index_of(Target { seg: 1, word: Some(1) }).unwrap();
        assert_eq!(t, 4);
        let tail = script.tail(2);
        assert_eq!(tail.text, "vectores ve sub uno y");
        assert_eq!(tail.first(), Some(Target { seg: 0, word: Some(2) }));
        assert_eq!(tail.locate(tail.text.find("uno").unwrap()), Some(Target { seg: 1, word: Some(1) }));
        assert!(script.tail(6).is_empty());
    }

    #[test]
    fn utf16() {
        let t = "más 𝑣 y";
        assert_eq!(utf16_to_byte(t, 0), 0);
        assert_eq!(utf16_to_byte(t, 4), "más ".len());
        assert_eq!(utf16_to_byte(t, 7), "más 𝑣 ".len());
    }
}
