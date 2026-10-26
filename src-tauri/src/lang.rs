//! Detección rápida de idioma (español / inglés) por palabras frecuentes.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Es,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Es => "es",
            Lang::En => "en",
        }
    }
}

const ES: &[&str] = &[
    "de", "la", "que", "el", "en", "y", "los", "del", "se", "las", "por", "un", "para", "con",
    "una", "su", "al", "lo", "como", "más", "pero", "sus", "le", "ya", "este", "esta", "entre",
    "cuando", "muy", "sin", "sobre", "también", "es", "son", "está", "están", "donde", "cual",
    "cada", "si", "hay", "fue", "ser", "nos", "sea", "dos", "uno",
];

const EN: &[&str] = &[
    "the", "of", "and", "to", "in", "is", "that", "for", "it", "with", "as", "was", "on", "are",
    "be", "by", "this", "an", "which", "or", "from", "at", "can", "we", "between", "where",
    "each", "if", "not", "there", "has", "have", "its", "these", "then", "let", "given", "such",
    "two", "one",
];

fn scores(text: &str) -> (u32, u32) {
    let (mut es, mut en) = (0, 0);
    for raw in text.split(|c: char| !c.is_alphabetic()) {
        if raw.is_empty() {
            continue;
        }
        let w = raw.to_lowercase();
        if ES.contains(&w.as_str()) {
            es += 1;
        }
        if EN.contains(&w.as_str()) {
            en += 1;
        }
        if w.chars().any(|c| matches!(c, 'á' | 'é' | 'í' | 'ó' | 'ú' | 'ñ')) {
            es += 1;
        }
    }
    if text.contains('¿') || text.contains('¡') {
        es += 2;
    }
    (es, en)
}

/// Devuelve un idioma sólo cuando hay evidencia clara.
pub fn detect(text: &str) -> Option<Lang> {
    let (es, en) = scores(text);
    if es >= en + 2 {
        Some(Lang::Es)
    } else if en >= es + 2 {
        Some(Lang::En)
    } else {
        None
    }
}

/// Decisión forzada cuando ya no se puede esperar más texto.
pub fn decide(text: &str, default: Lang) -> Lang {
    let (es, en) = scores(text);
    match es.cmp(&en) {
        std::cmp::Ordering::Greater => Lang::Es,
        std::cmp::Ordering::Less => Lang::En,
        std::cmp::Ordering::Equal => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecta_espanol() {
        assert_eq!(
            detect("La similitud del coseno entre los vectores estaría definida como"),
            Some(Lang::Es)
        );
    }

    #[test]
    fn detecta_ingles() {
        assert_eq!(
            detect("The cosine similarity between the vectors is defined as"),
            Some(Lang::En)
        );
    }

    #[test]
    fn sin_evidencia() {
        assert_eq!(detect("Similitud coseno"), None);
        assert_eq!(decide("Similitud coseno", Lang::En), Lang::En);
        assert_eq!(decide("Similitud del coseno", Lang::En), Lang::Es);
    }
}
