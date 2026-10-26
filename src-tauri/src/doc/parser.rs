//! Segmentador incremental de la salida del LLM (Markdown + LaTeX).
//!
//! Recibe los tokens tal como llegan por streaming y emite segmentos completos
//! en cuanto se puede decidir su límite, de modo que la lectura empiece antes
//! de que el modelo termine. El resultado no depende de cómo se partan los tokens.

use super::model::{SegKind, Segment};

pub struct StreamParser {
    buf: String,
    next_id: usize,
    at_line_start: bool,
    pending_para: bool,
}

enum Step {
    Emit(Segment),
    Consumed,
    NeedMore,
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamParser {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            next_id: 0,
            at_line_start: true,
            pending_para: true,
        }
    }

    pub fn push(&mut self, delta: &str) -> Vec<Segment> {
        self.buf.push_str(delta);
        self.drain(false)
    }

    pub fn finish(&mut self) -> Vec<Segment> {
        let out = self.drain(true);
        self.buf.clear();
        out
    }

    fn drain(&mut self, eof: bool) -> Vec<Segment> {
        let mut out = Vec::new();
        loop {
            match self.step(eof) {
                Step::Emit(s) => out.push(s),
                Step::Consumed => {}
                Step::NeedMore => break,
            }
        }
        out
    }

    fn consume(&mut self, n: usize) -> String {
        self.buf.drain(..n).collect()
    }

    fn take_para(&mut self) -> bool {
        std::mem::replace(&mut self.pending_para, false)
    }

    /// Los ids sólo avanzan cuando de verdad se emite un segmento (id == índice).
    fn emit(&mut self, seg: Option<Segment>) -> Step {
        match seg {
            Some(s) => {
                self.next_id += 1;
                Step::Emit(s)
            }
            None => Step::Consumed,
        }
    }

    fn line_end(&self, eof: bool) -> Option<usize> {
        match self.buf.find('\n') {
            Some(i) => Some(i),
            None if eof => Some(self.buf.len()),
            None => None,
        }
    }

    fn step(&mut self, eof: bool) -> Step {
        let Some(c) = self.buf.chars().next() else {
            return Step::NeedMore;
        };
        if c == '\n' {
            self.consume(1);
            if self.at_line_start {
                self.pending_para = true;
            }
            self.at_line_start = true;
            return Step::Consumed;
        }
        if c.is_whitespace() {
            self.consume(c.len_utf8());
            return Step::Consumed;
        }
        if self.at_line_start {
            if let Some(step) = self.line_start(eof) {
                return step;
            }
        }
        if let Some(step) = self.math(eof) {
            return step;
        }
        self.text(eof)
    }

    /// Construcciones que sólo cuentan al inicio de línea: cercas, encabezados y listas.
    fn line_start(&mut self, eof: bool) -> Option<Step> {
        const PREFIXES: [&str; 5] = ["```", "- ", "* ", "+ ", "**"];
        let b = self.buf.as_str();
        if !eof && PREFIXES.iter().any(|p| p.len() > b.len() && p.starts_with(b)) {
            return Some(Step::NeedMore);
        }

        if b.starts_with("```") {
            return Some(match self.line_end(eof) {
                Some(end) => {
                    self.consume(end);
                    Step::Consumed
                }
                None => Step::NeedMore,
            });
        }

        if b.starts_with('#') {
            let Some(end) = self.line_end(eof) else {
                return Some(Step::NeedMore);
            };
            let line = self.consume(end);
            self.at_line_start = false;
            let seg = Segment::text(self.next_id, SegKind::Heading, line.trim_start_matches('#'), true);
            self.pending_para = true;
            return Some(self.emit(seg));
        }

        if b.starts_with("- ") || b.starts_with("* ") || b.starts_with("+ ") {
            self.consume(2);
            self.pending_para = true;
            self.at_line_start = false;
            return Some(Step::Consumed);
        }

        // Una línea completa en negritas se trata como encabezado.
        if b.starts_with("**") {
            let Some(end) = self.line_end(eof) else {
                return Some(Step::NeedMore);
            };
            let line = self.buf[..end].trim_end();
            let inner = &line[2..];
            if line.len() > 4
                && inner.ends_with("**")
                && !inner[..inner.len() - 2].contains("**")
                && !line.contains('$')
            {
                let title = inner[..inner.len() - 2].to_owned();
                self.consume(end);
                self.at_line_start = false;
                let seg = Segment::text(self.next_id, SegKind::Heading, &title, true);
                self.pending_para = true;
                return Some(self.emit(seg));
            }
        }
        None
    }

    fn math(&mut self, eof: bool) -> Option<Step> {
        let b = self.buf.as_str();
        let (open, close, block) = if b.starts_with("$$") {
            ("$$", "$$", true)
        } else if b.starts_with("\\[") {
            ("\\[", "\\]", true)
        } else if b.starts_with("\\(") {
            ("\\(", "\\)", false)
        } else if b.starts_with('$') {
            if b.len() == 1 && !eof {
                return Some(Step::NeedMore);
            }
            ("$", "$", false)
        } else if b == "\\" && !eof {
            return Some(Step::NeedMore);
        } else {
            return None;
        };

        let body = &b[open.len()..];
        let (latex, consumed) = match find_close(body, close) {
            Some(i) => (body[..i].to_owned(), open.len() + i + close.len()),
            None if eof => (body.to_owned(), b.len()),
            None => return Some(Step::NeedMore),
        };
        self.consume(consumed);
        self.at_line_start = false;
        let para = if block {
            self.pending_para = false;
            true
        } else {
            self.take_para()
        };
        let seg = Segment::math(self.next_id, &latex, block, para);
        if block {
            self.pending_para = true;
        }
        Some(self.emit(seg))
    }

    fn text(&mut self, eof: bool) -> Step {
        let b = self.buf.as_str();
        let bytes = b.as_bytes();
        let mut end = None;
        let mut chars = b.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '$' | '\n' => {
                    end = Some(i);
                    break;
                }
                '\\' => match bytes.get(i + 1) {
                    Some(b'(') | Some(b'[') => {
                        end = Some(i);
                        break;
                    }
                    None if !eof => return Step::NeedMore,
                    _ => {}
                },
                '.' | '?' | '!' | ';' | ':' => match chars.peek() {
                    Some((_, n)) if n.is_whitespace() => {
                        end = Some(i + 1);
                        break;
                    }
                    None if !eof => return Step::NeedMore,
                    _ => {}
                },
                _ => {}
            }
        }
        let end = match end {
            Some(e) => e,
            None if eof => b.len(),
            None => return Step::NeedMore,
        };
        if end == 0 {
            // No debería ocurrir; evita un bucle infinito ante entradas raras.
            let n = b.chars().next().map_or(1, char::len_utf8);
            self.consume(n);
            return Step::Consumed;
        }
        let raw = self.consume(end);
        self.at_line_start = false;
        let para = self.take_para();
        let seg = Segment::text(self.next_id, SegKind::Text, &raw, para);
        if seg.is_none() && para {
            self.pending_para = true;
        }
        self.emit(seg)
    }
}

/// Busca el cierre de una fórmula ignorando `\$` escapados.
fn find_close(s: &str, pat: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = s[from..].find(pat) {
        let at = from + i;
        if pat == "$" && at > 0 && s.as_bytes()[at - 1] == b'\\' {
            from = at + 1;
            continue;
        }
        return Some(at);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const COSENO: &str = "Similitud del coseno\n\nLa similitud del coseno entre los vectores $\\mathbf{v}_1$ y $\\mathbf{v}_2$ estaría definida como:\n\n$$s_{\\theta}(\\mathbf{v}_1, \\mathbf{v}_2) = \\frac{\\mathbf{v}_1^T\\mathbf{v}_2}{\\|\\mathbf{v}_1\\|\\|\\mathbf{v}_2\\|} = 0.6682.$$";

    fn all_at_once(s: &str) -> Vec<Segment> {
        let mut p = StreamParser::new();
        let mut v = p.push(s);
        v.extend(p.finish());
        v
    }

    fn char_by_char(s: &str) -> Vec<Segment> {
        let mut p = StreamParser::new();
        let mut v = Vec::new();
        for c in s.chars() {
            v.extend(p.push(&c.to_string()));
        }
        v.extend(p.finish());
        v
    }

    fn summary(v: &[Segment]) -> Vec<(SegKind, String, bool, bool)> {
        v.iter()
            .map(|s| (s.kind, s.display.clone(), s.block, s.para))
            .collect()
    }

    #[test]
    fn ejemplo_coseno() {
        let v = all_at_once(COSENO);
        let got = summary(&v);
        assert_eq!(got.len(), 7, "{got:#?}");
        assert_eq!(got[0], (SegKind::Text, "Similitud del coseno".into(), false, true));
        assert_eq!(
            got[1],
            (SegKind::Text, "La similitud del coseno entre los vectores".into(), false, true)
        );
        assert_eq!(got[2], (SegKind::Math, "\\mathbf{v}_1".into(), false, false));
        assert_eq!(got[3], (SegKind::Text, "y".into(), false, false));
        assert_eq!(got[4].0, SegKind::Math);
        assert_eq!(got[5], (SegKind::Text, "estaría definida como:".into(), false, false));
        assert_eq!(got[6].0, SegKind::Math);
        assert!(got[6].2 && got[6].3);
        assert!(got[6].1.ends_with("= 0.6682."));
        for (i, s) in v.iter().enumerate() {
            assert_eq!(s.id, i);
        }
    }

    #[test]
    fn streaming_es_equivalente() {
        for doc in [
            COSENO,
            "# Título\nTexto normal. Otra oración? Sí.\n\n- elemento uno\n- elemento $x^2$\n",
            "```markdown\n**Teorema**\nSea \\(f\\) continua en \\[a,b\\].\n```",
        ] {
            assert_eq!(all_at_once(doc), char_by_char(doc), "doc: {doc}");
        }
    }

    #[test]
    fn dolar_partido_entre_tokens() {
        let mut p = StreamParser::new();
        let mut v = p.push("Sea $");
        v.extend(p.push("$x"));
        v.extend(p.push("^2$"));
        v.extend(p.push("$ fin."));
        v.extend(p.finish());
        let got = summary(&v);
        assert_eq!(got[0], (SegKind::Text, "Sea".into(), false, true));
        assert_eq!(got[1], (SegKind::Math, "x^2".into(), true, true));
        assert_eq!(got[2], (SegKind::Text, "fin.".into(), false, true));
    }

    #[test]
    fn encabezados_y_cercas() {
        let v = all_at_once("```markdown\n## Similitud\n**Definición**\nTexto **importante** aquí.\n```\n");
        let got = summary(&v);
        assert_eq!(got[0], (SegKind::Heading, "Similitud".into(), false, true));
        assert_eq!(got[1], (SegKind::Heading, "Definición".into(), false, true));
        assert_eq!(got[2], (SegKind::Text, "Texto importante aquí.".into(), false, true));
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn delimitadores_latex_alternativos() {
        let got = summary(&all_at_once("Sea \\(v_1\\) y \\[\\sum_i x_i\\]"));
        assert_eq!(got[1], (SegKind::Math, "v_1".into(), false, false));
        assert_eq!(got[3], (SegKind::Math, "\\sum_i x_i".into(), true, true));
    }

    #[test]
    fn decimales_no_parten_oraciones() {
        let got = summary(&all_at_once("El valor es 0.6682 aproximadamente."));
        assert_eq!(got.len(), 1);
    }
}
