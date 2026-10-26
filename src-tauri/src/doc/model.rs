use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SegKind {
    Heading,
    Text,
    Math,
}

/// Unidad mínima del documento transcrito: un trozo de texto o una fórmula.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Segment {
    pub id: usize,
    pub kind: SegKind,
    /// Texto plano (encabezado / texto) o LaTeX (matemática).
    pub display: String,
    /// Palabras del texto; vacío en matemáticas. `display == words.join(" ")`.
    pub words: Vec<String>,
    /// Fórmula en bloque (`$$..$$`).
    pub block: bool,
    /// Empieza un párrafo nuevo.
    pub para: bool,
}

impl Segment {
    pub fn text(id: usize, kind: SegKind, raw: &str, para: bool) -> Option<Segment> {
        let cleaned: String = raw.chars().filter(|c| !matches!(c, '*' | '`')).collect();
        let words: Vec<String> = cleaned.split_whitespace().map(str::to_owned).collect();
        if words.is_empty() {
            return None;
        }
        Some(Segment {
            id,
            kind,
            display: words.join(" "),
            words,
            block: false,
            para,
        })
    }

    pub fn math(id: usize, latex: &str, block: bool, para: bool) -> Option<Segment> {
        let latex = latex.trim();
        if latex.is_empty() {
            return None;
        }
        Some(Segment {
            id,
            kind: SegKind::Math,
            display: latex.to_owned(),
            words: Vec::new(),
            block,
            para,
        })
    }

    /// El segmento cierra una oración (sirve para partir la lectura en fragmentos).
    pub fn ends_sentence(&self) -> bool {
        self.kind != SegKind::Math
            && self
                .display
                .chars()
                .last()
                .is_some_and(|c| matches!(c, '.' | '?' | '!' | ';' | ':'))
    }
}
