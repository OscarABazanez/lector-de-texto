//! Marca en el LaTeX qué fragmento lee cada frase, para resaltarlo mientras suena.
//!
//! Cada fragmento se envuelve con `\htmlData{part=N}{…}`; KaTeX conserva el tipo de símbolo
//! dentro de `\htmlData`, así que la fórmula se dibuja igual (verificado con KaTeX 0.19
//! comparando márgenes y posiciones verticales en 120 lecturas reales).
//!
//! El modelo no siempre copia los fragmentos con exactitud (no admite `temperature: 0`): a veces
//! cambia llaves (`v_1^{T}` por `v_1^T`), arrastra estructura (`\frac{v_1^T v_2}{`) o da un
//! exponente suelto (`^2`). Por eso se busca ignorando llaves, se recorta la estructura sobrante,
//! los exponentes sueltos se buscan dentro de la frase anterior y se descarta cualquier rango que
//! dejaría a KaTeX con un comando incompleto.

use serde::Serialize;

/// Qué pasó con cada frase de la lectura.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PartKind {
    /// Su fragmento quedó marcado con `part=i`.
    Part,
    /// Sin fragmento propio: palabras de enlace («entre», «por»), algo ya marcado o el cierre
    /// de un grupo (`\right]`, `\end{bmatrix}`).
    Link,
    /// Tenía fragmento pero no se pudo marcar.
    Lost,
}

/// Estructura que se recorta de los extremos de un fragmento mal copiado.
const JUNK: &[&str] = &["{", "}", "\\frac", "\\dfrac", "\\tfrac", "\\left", "\\right", "&", "\\\\", "_", "^"];

/// Tokens que no cuentan como contenido al revisar si la lectura se saltó algo.
const PUNCT: &[&str] = &[".", ",", ";", ":", "(", ")", "{", "}", "\\,", "\\;", "\\!", "\\quad"];

/// Estructura que no se pronuncia (tampoco cuenta como contenido).
const SILENT: &[&str] = &[
    "&", "\\\\", "\\begin", "\\end", "\\mathbf", "\\mathrm", "\\mathit", "\\boldsymbol", "\\displaystyle",
];

/// Número de argumentos obligatorios de los comandos que los tienen.
fn arity(cmd: &str) -> Option<usize> {
    match cmd {
        "\\frac" | "\\dfrac" | "\\tfrac" | "\\binom" => Some(2),
        "\\sqrt" | "\\mathbf" | "\\mathrm" | "\\mathit" | "\\mathcal" | "\\mathbb" | "\\boldsymbol"
        | "\\hat" | "\\bar" | "\\vec" | "\\tilde" | "\\dot" | "\\ddot" | "\\overline"
        | "\\underline" | "\\text" | "\\operatorname" => Some(1),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
struct Tok<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

/// Comandos (`\nombre`), símbolos escapados (`\|`, `\,`) y caracteres sueltos; sin espacios.
fn tokens(s: &str) -> Vec<Tok<'_>> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = s[i..].chars().next() {
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let mut j = i + c.len_utf8();
        if c == '\\' {
            while j < s.len() && bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j == i + 1 {
                j += s[j..].chars().next().map_or(0, char::len_utf8);
            }
        }
        out.push(Tok { text: &s[i..j], start: i, end: j });
        i = j;
    }
    out
}

fn is_script(text: &str) -> bool {
    matches!(text, "_" | "^")
}

/// Profundidad final de llaves en `[a, b)` y la mínima alcanzada.
fn depth_profile(t: &[Tok], a: usize, b: usize) -> (i32, i32) {
    let (mut depth, mut lowest) = (0, 0);
    for tok in &t[a..b] {
        match tok.text {
            "{" => depth += 1,
            "}" => depth -= 1,
            _ => {}
        }
        lowest = lowest.min(depth);
    }
    (depth, lowest)
}

/// Fin (exclusivo) del argumento que empieza en `k`: un grupo entre llaves o un token.
fn arg_end(t: &[Tok], mut k: usize) -> Option<usize> {
    if k >= t.len() {
        return None;
    }
    if t[k].text != "{" {
        return Some(k + 1);
    }
    let mut depth = 0;
    while k < t.len() {
        match t[k].text {
            "{" => depth += 1,
            "}" => depth -= 1,
            _ => {}
        }
        k += 1;
        if depth == 0 {
            return Some(k);
        }
    }
    None
}

/// Fin (exclusivo) de los `_…` y `^…` que siguen a la posición `k`.
fn scripts_end(t: &[Tok], mut k: usize) -> usize {
    while k < t.len() && is_script(t[k].text) {
        match arg_end(t, k + 1) {
            Some(e) => k = e,
            None => return k,
        }
    }
    k
}

/// `\left[`, `\big(`…: el token siguiente es un delimitador y KaTeX no admite otra cosa en su lugar.
fn takes_delimiter(cmd: &str) -> bool {
    matches!(cmd, "\\left" | "\\right" | "\\middle")
        || matches!(cmd.trim_end_matches(['l', 'r', 'm']), "\\big" | "\\Big" | "\\bigg" | "\\Bigg")
}

/// Ningún comando del rango deja argumentos, `\left`/`\right`, entornos o scripts fuera de él.
fn self_contained(t: &[Tok], a: usize, b: usize) -> bool {
    let (mut left_right, mut env) = (0, 0);
    for i in a..b {
        let text = t[i].text;
        match text {
            "\\left" => left_right += 1,
            "\\right" => left_right -= 1,
            "\\begin" => env += 1,
            "\\end" => env -= 1,
            // Separadores de celdas y filas: sólo dentro de una matriz completa del rango.
            "&" | "\\\\" if env == 0 => return false,
            _ => {}
        }
        if left_right < 0 || env < 0 || (takes_delimiter(text) && i + 1 >= b) {
            return false;
        }
        if let Some(n) = arity(text) {
            let mut j = i + 1;
            if text == "\\sqrt" && j < b && t[j].text == "[" {
                while j < b && t[j].text != "]" {
                    j += 1;
                }
                j += 1;
            }
            for _ in 0..n {
                match arg_end(t, j) {
                    Some(e) if e <= b => j = e,
                    _ => return false,
                }
            }
        }
        if is_script(text) && i + 1 >= b {
            return false;
        }
    }
    left_right == 0 && env == 0
}

/// El token `i` es parte del nombre de un entorno (`bmatrix` en `\begin{bmatrix}`).
fn in_env_name(t: &[Tok], i: usize) -> bool {
    (0..i).any(|k| matches!(t[k].text, "\\begin" | "\\end") && arg_end(t, k + 1).is_some_and(|e| i < e))
}

/// Símbolos que se leen en voz alta: sin puntuación, `_`/`^`, `\left[`, `&`, nombres de entorno…
/// Así una lectura completa de un vector o una matriz no parece omitir nada.
fn is_content(t: &[Tok], i: usize) -> bool {
    let text = t[i].text;
    let silent = PUNCT.contains(&text) || SILENT.contains(&text) || is_script(text) || takes_delimiter(text);
    !silent && !(i > 0 && takes_delimiter(t[i - 1].text)) && !in_env_name(t, i)
}

/// Si `[a, b)` es sólo la apertura de un grupo (`\left[` o `\begin{bmatrix}`), el fin del
/// grupo completo con su cierre: «el vector» o «la matriz» resaltan todo.
fn group_end(t: &[Tok], a: usize, b: usize) -> Option<usize> {
    let (open, close, end): (&str, &str, fn(&[Tok], usize) -> Option<usize>) = match t[a].text {
        "\\left" if b == a + 2 => ("\\left", "\\right", |t, k| (k + 2 <= t.len()).then_some(k + 2)),
        "\\begin" if arg_end(t, a + 1) == Some(b) => ("\\begin", "\\end", |t, k| arg_end(t, k + 1)),
        _ => return None,
    };
    let mut depth = 0;
    for k in a..t.len() {
        if t[k].text == open {
            depth += 1;
        } else if t[k].text == close {
            depth -= 1;
            if depth == 0 {
                return end(t, k);
            }
        }
    }
    None
}

/// `[a, b)` es sólo el cierre de un grupo (`\right]`, `\end{bmatrix}`) o un delimitador de
/// `\right`, `\big`…: no tiene fragmento propio que marcar.
fn is_closing(t: &[Tok], a: usize, b: usize) -> bool {
    match t[a].text {
        "\\right" => b <= a + 2,
        "\\end" => arg_end(t, a + 1) == Some(b),
        _ => a > 0 && takes_delimiter(t[a - 1].text),
    }
}

/// El rango toca el nombre de un entorno (`\begin{bmatrix}`: la `a` de `bmatrix` no es la variable `a`).
fn touches_env_name(t: &[Tok], a: usize, b: usize) -> bool {
    (0..t.len()).filter(|&k| matches!(t[k].text, "\\begin" | "\\end")).any(|k| {
        let Some(e) = arg_end(t, k + 1) else { return false };
        let touches_name = a < e && b > k + 1;
        touches_name && !(a <= k && e <= b)
    })
}

/// Justo tras `_`, `^` o un comando con argumentos (sin llaves) el fragmento es ese argumento.
fn is_bare_arg(t: &[Tok], a: usize) -> bool {
    a > 0 && (is_script(t[a - 1].text) || arity(t[a - 1].text).is_some())
}

/// Posiciones donde aparece `pat` comparando sin llaves; cada rango se extiende a las llaves
/// vecinas hasta quedar balanceado. `last` es el último fragmento marcado: se busca primero
/// después de él (o dentro, con `inside_first`) y al final en el resto.
fn find(t: &[Tok], pat: &[&str], last: (usize, usize), inside_first: bool) -> Vec<(usize, usize)> {
    let nb: Vec<usize> = (0..t.len()).filter(|&i| !matches!(t[i].text, "{" | "}")).collect();
    let seq: Vec<&str> = nb.iter().map(|&i| t[i].text).collect();
    let pat: Vec<&str> = pat.iter().copied().filter(|p| !matches!(*p, "{" | "}")).collect();
    if pat.is_empty() || pat.len() > seq.len() {
        return Vec::new();
    }
    let n = seq.len() - pat.len() + 1;
    let (after, rest): (Vec<usize>, Vec<usize>) = (0..n).partition(|&k| nb[k] >= last.1);
    let (inside, rest): (Vec<usize>, Vec<usize>) = rest.into_iter().partition(|&k| nb[k] >= last.0);
    let order = if inside_first { [inside, after, rest] } else { [after, inside, rest] };
    let mut out = Vec::new();
    for k in order.into_iter().flatten() {
        if seq[k..k + pat.len()] != pat[..] {
            continue;
        }
        let (mut a, mut b) = (nb[k], nb[k + pat.len() - 1] + 1);
        for _ in 0..8 {
            let (depth, lowest) = depth_profile(t, a, b);
            if lowest < 0 && a > 0 && t[a - 1].text == "{" {
                a -= 1;
            } else if depth > 0 && b < t.len() && t[b].text == "}" {
                b += 1;
            } else {
                break;
            }
        }
        out.push((a, b));
    }
    out
}

/// Vale la pena dividir la lectura: las variables sueltas (`v_1`, `x^2`) se leen de una vez.
pub fn needs_parts(latex: &str) -> bool {
    tokens(latex).iter().filter(|t| !matches!(t.text, "{" | "}")).count() >= 6
}

/// Resultado del marcado de una lectura.
#[derive(Debug, PartialEq)]
pub struct Annotation {
    /// LaTeX con `\htmlData{part=i}`; `None` si no se marcó nada.
    pub latex: Option<String>,
    pub kinds: Vec<PartKind>,
    /// Rangos marcados (tokens `[a, b)`, número de frase).
    ranges: Vec<(usize, usize, usize)>,
    /// Índices de los tokens con contenido (sin puntuación ni llaves).
    content: Vec<usize>,
}

impl Annotation {
    /// Señales de que la lectura se saltó parte de la fórmula: una frase corta que cubre muchos
    /// símbolos sin frases internas («raíz cuadrada de» sobre toda la raíz, «la norma dos de» sin
    /// «x menos y») o muchos símbolos que ninguna frase cubre. En 60 lecturas reales sólo saltó
    /// con lecturas que sí omitían algo.
    pub fn omissions(&self, says: &[String]) -> usize {
        let covered = |i: usize, ranges: &[(usize, usize)]| ranges.iter().any(|&(s, e)| s <= i && i < e);
        let all: Vec<(usize, usize)> = self.ranges.iter().map(|&(s, e, _)| (s, e)).collect();
        let mut bad = 0;
        for &(a, b, idx) in &self.ranges {
            let inner: Vec<(usize, usize)> = self
                .ranges
                .iter()
                .filter(|&&(s, e, j)| j != idx && a <= s && e <= b)
                .map(|&(s, e, _)| (s, e))
                .collect();
            let own = self.content.iter().filter(|&&i| a <= i && i < b && !covered(i, &inner)).count();
            // Las frases de enlace que siguen (límites, «entre») también leen esta parte.
            let count = |j: usize| says.get(j).map_or(0, |s| s.split_whitespace().count());
            let mut words = count(idx);
            let mut j = idx + 1;
            while self.kinds.get(j) == Some(&PartKind::Link) {
                words += count(j);
                j += 1;
            }
            // Una lectura completa usa ~1 palabra por símbolo; menos de 0.7 es sospechoso.
            if own >= 5 && 10 * words < 7 * own {
                bad += 1;
            }
        }
        let uncovered = self.content.iter().filter(|&&i| !covered(i, &all)).count();
        if uncovered >= 6 {
            bad += 1;
        }
        bad
    }

    pub fn lost(&self) -> usize {
        self.kinds.iter().filter(|k| **k == PartKind::Lost).count()
    }
}

/// El rango no está balanceado, no es autocontenido o se cruza con otro ya marcado.
/// `Err(true)` si coincide exactamente con uno existente.
fn check_range(t: &[Tok], ranges: &[(usize, usize, usize)], a: usize, b: usize) -> Result<(), bool> {
    let (depth, lowest) = depth_profile(t, a, b);
    if depth != 0 || lowest < 0 || !self_contained(t, a, b) || touches_env_name(t, a, b) {
        return Err(false);
    }
    if ranges.iter().any(|&(s, e, _)| s == a && e == b) {
        return Err(true);
    }
    let crosses = ranges.iter().any(|&(s, e, _)| {
        let nested = (a <= s && e <= b) || (s <= a && b <= e);
        let disjoint = b <= s || a >= e;
        !(nested || disjoint)
    });
    if crosses {
        Err(false)
    } else {
        Ok(())
    }
}

/// Envuelve el fragmento de cada frase `texs[i]` con `\htmlData{part=i}{…}`.
pub fn annotate(latex: &str, texs: &[&str]) -> Annotation {
    let t = tokens(latex);
    let content: Vec<usize> = (0..t.len()).filter(|&i| is_content(&t, i)).collect();
    let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
    let mut kinds = Vec::with_capacity(texs.len());
    // Último fragmento marcado: el orden de lectura sigue desde ahí.
    let mut last = (0, 0);
    for (idx, tex) in texs.iter().enumerate() {
        let pat: Vec<&str> = tokens(tex).iter().map(|x| x.text).collect();
        let mut trimmed: &[&str] = &pat;
        while trimmed.first().is_some_and(|f| JUNK.contains(f)) {
            trimmed = &trimmed[1..];
        }
        while trimmed.last().is_some_and(|l| JUNK.contains(l)) {
            trimmed = &trimmed[..trimmed.len() - 1];
        }
        if trimmed.is_empty() {
            kinds.push(PartKind::Link);
            continue;
        }

        if is_script(pat[0]) {
            // Un exponente o subíndice suelto («al cuadrado») se refiere a la base recién leída:
            // se busca primero dentro de la frase anterior y se marca sólo su argumento.
            let mut placed = None;
            for (a, b) in find(&t, &pat, last, true) {
                if !is_script(t[a].text) || arg_end(&t, a + 1) != Some(b) {
                    continue;
                }
                let (a2, b2) = if t[a + 1].text == "{" { (a + 2, b - 1) } else { (a + 1, b) };
                if a2 < b2 && check_range(&t, &ranges, a2, b2).is_ok() {
                    placed = Some((a2, b2));
                    break;
                }
            }
            // Sin lugar propio (p. ej. los límites `_0^{\infty}` ya marcados con su integral).
            kinds.push(match placed {
                Some((a, b)) => {
                    ranges.push((a, b, idx));
                    last = (a, b);
                    PartKind::Part
                }
                None => PartKind::Link,
            });
            continue;
        }

        // El fragmento tal cual y, si no sirve, sin la estructura sobrante de los extremos.
        let mut candidates = find(&t, &pat, last, false);
        if trimmed.len() != pat.len() {
            candidates.extend(find(&t, trimmed, last, false));
        }
        let mut placed = None;
        let mut duplicate = false;
        for (mut a, mut b) in candidates {
            if is_script(t[a].text) {
                continue;
            }
            // El corchete de `\left[` no se puede envolver solo (KaTeX sólo admite un delimitador
            // ahí): «abrimos corchete» o «el vector» marcan todo el grupo.
            if a > 0 && t[a - 1].text == "\\left" && b == a + 1 {
                a -= 1;
            }
            if let Some(end) = group_end(&t, a, b) {
                b = end;
            }
            // El cierre queda con el resaltado anterior; se para la búsqueda para no marcar
            // otro corchete que no es el que se lee.
            if is_closing(&t, a, b) {
                duplicate = true;
                break;
            }
            // La base va con sus `_` y `^`: marcarla sola cambia dónde se dibujan.
            if b < t.len() && is_script(t[b].text) {
                b = scripts_end(&t, b);
            }
            if is_bare_arg(&t, a) && b - a > 1 {
                continue;
            }
            match check_range(&t, &ranges, a, b) {
                Ok(()) => {
                    placed = Some((a, b));
                    break;
                }
                Err(same) => duplicate |= same,
            }
        }
        kinds.push(match placed {
            Some((a, b)) => {
                ranges.push((a, b, idx));
                last = (a, b);
                PartKind::Part
            }
            // Repite un fragmento ya marcado o lee un cierre (`\right]`): se queda el resaltado anterior.
            None if duplicate => PartKind::Link,
            None => PartKind::Lost,
        });
    }
    // Una frase que marcó toda una fracción, cuando el denominador tiene frases propias y el
    // numerador no, en realidad lee el numerador: su marca se mueve ahí.
    for n in 0..ranges.len() {
        let (a, b, idx) = ranges[n];
        if !matches!(t[a].text, "\\frac" | "\\dfrac" | "\\tfrac") || a + 1 >= t.len() || t[a + 1].text != "{" {
            continue;
        }
        let Some(ea) = arg_end(&t, a + 1) else { continue };
        if ea >= b || t[ea].text != "{" || arg_end(&t, ea) != Some(b) {
            continue;
        }
        let inner = |s0: usize, e0: usize| ranges.iter().any(|&(s, e, j)| j != idx && s0 <= s && e <= e0);
        let (na, nb) = (a + 2, ea - 1);
        let retarget = na < nb
            && !inner(a + 1, ea)
            && inner(ea, b)
            && !ranges.iter().any(|&(s, e, _)| s == na && e == nb);
        if retarget {
            ranges[n] = (na, nb, idx);
        }
    }
    if ranges.is_empty() {
        return Annotation { latex: None, kinds, ranges, content };
    }

    // Inserciones por posición en bytes: (posición, tamaño del rango, texto).
    let mut opens: Vec<(usize, usize, String)> = Vec::new();
    let mut closes: Vec<(usize, usize, &str)> = Vec::new();
    for &(a, b, idx) in &ranges {
        let bare = is_bare_arg(&t, a);
        opens.push((t[a].start, b - a, format!("{}\\htmlData{{part={idx}}}{{", if bare { "{" } else { "" })));
        closes.push((t[b - 1].end, b - a, if bare { "}}" } else { "}" }));
    }
    let mut points: Vec<usize> = opens.iter().map(|o| o.0).chain(closes.iter().map(|c| c.0)).collect();
    points.sort_unstable();
    points.dedup();

    let mut out = String::with_capacity(latex.len() + ranges.len() * 24);
    let mut prev = 0;
    for p in points {
        out.push_str(&latex[prev..p]);
        prev = p;
        // En el mismo punto: primero se cierran las internas y luego se abren las externas.
        let mut cs: Vec<_> = closes.iter().filter(|c| c.0 == p).collect();
        cs.sort_by_key(|c| c.1);
        cs.iter().for_each(|c| out.push_str(c.2));
        let mut os: Vec<_> = opens.iter().filter(|o| o.0 == p).collect();
        os.sort_by_key(|o| std::cmp::Reverse(o.1));
        os.iter().for_each(|o| out.push_str(&o.2));
    }
    out.push_str(&latex[prev..]);
    Annotation { latex: Some(out), kinds, ranges, content }
}

#[cfg(test)]
mod tests {
    use super::PartKind::{Link, Lost, Part};
    use super::*;

    fn ann(latex: &str, texs: &[&str]) -> Option<String> {
        annotate(latex, texs).latex
    }

    fn owned(says: &[&str]) -> Vec<String> {
        says.iter().map(|s| s.to_string()).collect()
    }

    /// Respuestas reales de Claude Haiku 5.5; las salidas esperadas se dibujaron con KaTeX
    /// y quedaron idénticas al original.
    #[test]
    fn respuestas_reales_de_haiku() {
        let cases: &[(&str, &[&str], &str)] = &[
            (
                r"s_\theta(v_1, v_2) = \frac{v_1^T v_2}{\|v_1\|_2 \|v_2\|_2} = 0.6682.",
                &[r"s_\theta(v_1, v_2)", "=", "v_1^T v_2", "", r"\|v_1\|_2", "", r"\|v_2\|_2", "=", "0.6682"],
                r"\htmlData{part=0}{s_\theta(v_1, v_2)} \htmlData{part=1}{=} \frac{\htmlData{part=2}{v_1^T v_2}}{\htmlData{part=4}{\|v_1\|_2} \htmlData{part=6}{\|v_2\|_2}} \htmlData{part=7}{=} \htmlData{part=8}{0.6682}.",
            ),
            (
                r"\int_0^{\infty} e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
                &[r"\int_0^{\infty}", "e^{-x^2}", r"\,dx", "=", r"\sqrt{\pi}", "", "2"],
                r"\htmlData{part=0}{\int_0^{\infty}} \htmlData{part=1}{e^{-x^2}}\htmlData{part=2}{\,dx} \htmlData{part=3}{=} \frac{\htmlData{part=4}{\sqrt{\pi}}}{\htmlData{part=6}{2}}",
            ),
            (
                r"\sum_{i=1}^{n} x_i^2",
                &[r"\sum", "i=1", "n", "x_i^2"],
                r"\htmlData{part=0}{\sum_{\htmlData{part=1}{i=1}}^{\htmlData{part=2}{n}}} \htmlData{part=3}{x_i^2}",
            ),
            (
                r"f'(x) = \lim_{h \to 0} \frac{f(x+h)-f(x)}{h}",
                &["f'(x)", r"\lim_{h \to 0}", r"h \to 0", "f(x+h)-f(x)", "h"],
                r"\htmlData{part=0}{f'(x)} = \htmlData{part=1}{\lim_{\htmlData{part=2}{h \to 0}}} \frac{\htmlData{part=3}{f(x+h)-f(x)}}{\htmlData{part=4}{h}}",
            ),
            ("v_1", &["v_1"], r"\htmlData{part=0}{v_1}"),
        ];
        for (latex, texs, expected) in cases {
            assert_eq!(ann(latex, texs).as_deref(), Some(*expected), "{latex}");
        }
        assert_eq!(annotate(cases[0].0, cases[0].1).kinds, [Part, Part, Part, Link, Part, Link, Part, Part, Part]);
    }

    /// Lectura real con raíces: primero la raíz completa y luego su contenido, con exponentes sueltos.
    #[test]
    fn raices_con_contenido() {
        let latex = r"\|x - y\|_2 = \sqrt{(x_1 - y_1)^2 + (x_2 - y_2)^2} = \sqrt{0.5^2 + 0.5^2} = 0.7071";
        let texs = [
            r"\|x - y\|_2", "x - y", "=", r"\sqrt{(x_1 - y_1)^2 + (x_2 - y_2)^2}", "(x_1 - y_1)^2", "^2", "+",
            "(x_2 - y_2)^2", "^2", "=", r"\sqrt{0.5^2 + 0.5^2}", "0.5^2", "+", "0.5^2", "=", "0.7071",
        ];
        let a = annotate(latex, &texs);
        assert_eq!(
            a.latex.as_deref(),
            Some(r"\htmlData{part=0}{\|\htmlData{part=1}{x - y}\|_2} \htmlData{part=2}{=} \htmlData{part=3}{\sqrt{\htmlData{part=4}{(x_1 - y_1)^{\htmlData{part=5}{2}}} \htmlData{part=6}{+} \htmlData{part=7}{(x_2 - y_2)^{\htmlData{part=8}{2}}}}} \htmlData{part=9}{=} \htmlData{part=10}{\sqrt{\htmlData{part=11}{0.5^2} \htmlData{part=12}{+} \htmlData{part=13}{0.5^2}}} \htmlData{part=14}{=} \htmlData{part=15}{0.7071}")
        );
        assert!(a.kinds.iter().all(|k| *k == Part));
        let says = owned(&[
            "la norma dos de x menos y", "x menos y", "igual a", "la raíz cuadrada de", "x sub uno menos y sub uno",
            "al cuadrado", "más", "x sub dos menos y sub dos", "al cuadrado", "igual a", "la raíz cuadrada de",
            "cero punto cinco al cuadrado", "más", "cero punto cinco al cuadrado", "igual a", "cero punto siete cero siete uno",
        ]);
        assert_eq!(a.omissions(&says), 0);
    }

    /// La lectura que provocó el fallo: «raíz cuadrada de» sin leer lo de adentro.
    #[test]
    fn detecta_contenido_omitido() {
        let latex = r"\|x - y\|_2 = \sqrt{(x_1 - y_1)^2 + (x_2 - y_2)^2} = \sqrt{0.5^2 + 0.5^2} = 0.7071";
        let texs = [r"\|x - y\|_2", "=", r"\sqrt{(x_1 - y_1)^2 + (x_2 - y_2)^2}", "=", r"\sqrt{0.5^2 + 0.5^2}", "=", "0.7071"];
        let says = owned(&[
            "la norma dos de x menos y", "igual a", "raíz cuadrada de", "igual a", "raíz cuadrada de", "igual a",
            "cero punto siete cero siete uno",
        ]);
        assert_eq!(annotate(latex, &texs).omissions(&says), 2);
    }

    /// Lecturas reales: «la norma dos de» sin su contenido sí es omisión; «la integral de» seguida
    /// del enlace «cero a infinito» (sus límites) no lo es.
    #[test]
    fn omisiones_cortas_y_enlaces() {
        let latex = r"\|x - y\|_2 = \sqrt{(x_1 - y_1)^2 + (x_2 - y_2)^2} = \sqrt{0.5^2 + 0.5^2} = 0.7071";
        let texs = [
            r"\|x - y\|_2", "=", r"\sqrt{(x_1 - y_1)^2 + (x_2 - y_2)^2}", "(x_1 - y_1)^2", "^2", "+", "(x_2 - y_2)^2", "^2",
            "=", r"\sqrt{0.5^2 + 0.5^2}", "0.5^2", "+", "0.5^2", "=", "0.7071",
        ];
        let says = owned(&[
            "la norma dos de", "igual", "la raíz cuadrada de", "x sub uno menos y sub uno", "al cuadrado", "más",
            "x sub dos menos y sub dos", "al cuadrado", "igual", "la raíz cuadrada de", "cero punto cinco al cuadrado",
            "más", "cero punto cinco al cuadrado", "igual", "cero punto siete cero siete uno",
        ]);
        assert_eq!(annotate(latex, &texs).omissions(&says), 1);

        let latex = r"\int_0^{\infty} e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}";
        let texs = [r"\int_0^{\infty} e^{-x^2}\,dx", r"_0^{\infty}", "e^{-x^2}", "e", "-x^2", r"\,dx", "=", r"\sqrt{\pi}", r"\pi", "", "2"];
        let says = owned(&[
            "la integral de", "cero a infinito", "e a la menos x al cuadrado", "e", "menos x al cuadrado",
            "diferencial de x", "igual a", "la raíz cuadrada de", "pi", "entre", "dos",
        ]);
        assert_eq!(annotate(latex, &texs).omissions(&says), 0);
    }

    /// Fragmentos mal copiados que devolvió Haiku en pruebas reales.
    #[test]
    fn repara_fragmentos_imperfectos() {
        // Estructura de la fracción arrastrada: se marca sólo el numerador.
        assert_eq!(
            ann(r"s = \frac{v_1^{T} v_2}{\|v\|}", &[r"\frac{v_1^{T} v_2}{"]).as_deref(),
            Some(r"s = \frac{\htmlData{part=0}{v_1^{T} v_2}}{\|v\|}")
        );
        // Llaves distintas a las del original.
        assert_eq!(
            ann(r"\frac{v_1^{T} v_2}{w}", &["v_1^T v_2"]).as_deref(),
            Some(r"\frac{\htmlData{part=0}{v_1^{T} v_2}}{w}")
        );
        // `\frac` sin denominador rompería KaTeX: se marca el numerador.
        assert_eq!(
            ann(r"\frac{v_1^T v_2}{w}", &[r"\frac{v_1^T v_2}"]).as_deref(),
            Some(r"\frac{\htmlData{part=0}{v_1^T v_2}}{w}")
        );
        // El operador de subíndice incluido en el fragmento.
        assert_eq!(
            ann(r"\sum_{i=1}^{n} x", &[r"\sum", "_{i=1}"]).as_deref(),
            Some(r"\htmlData{part=0}{\sum_{\htmlData{part=1}{i=1}}^{n}} x")
        );
    }

    /// Lectura real: el numerador vino con la fracción completa como fragmento.
    #[test]
    fn fraccion_completa_se_mueve_al_numerador() {
        let a = annotate(
            r"s_\theta(v_1, v_2) = \frac{v_1^T v_2}{\|v_1\|_2 \|v_2\|_2} = 0.6682.",
            &[r"s_\theta(v_1, v_2)", "=", r"\frac{v_1^T v_2}{\|v_1\|_2 \|v_2\|_2}", "", r"\|v_1\|_2", "", r"\|v_2\|_2", "=", "0.6682"],
        );
        assert_eq!(
            a.latex.as_deref(),
            Some(r"\htmlData{part=0}{s_\theta(v_1, v_2)} \htmlData{part=1}{=} \frac{\htmlData{part=2}{v_1^T v_2}}{\htmlData{part=4}{\|v_1\|_2} \htmlData{part=6}{\|v_2\|_2}} \htmlData{part=7}{=} \htmlData{part=8}{0.6682}.")
        );
    }

    #[test]
    fn exponentes_van_con_su_base() {
        // Marcar `(a)` sin su `^2` movería el exponente: la base se marca con él.
        assert_eq!(
            ann("(a)^2 + b", &["(a)", "^2", "+", "b"]).as_deref(),
            Some(r"\htmlData{part=0}{(a)^{\htmlData{part=1}{2}}} \htmlData{part=2}{+} \htmlData{part=3}{b}")
        );
    }

    #[test]
    fn repeticiones_y_limites_son_enlaces() {
        let a = annotate(
            r"\int_0^{\infty} e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
            &[r"\int_0^{\infty} e^{-x^2}\,dx", r"_0^{\infty}", "e^{-x^2}", "e", "-x^2", r"\,dx", "=", r"\frac{\sqrt{\pi}}{2}", r"\sqrt{\pi}", "", "2"],
        );
        assert_eq!(
            a.latex.as_deref(),
            Some(r"\htmlData{part=0}{\int_0^{\infty} \htmlData{part=2}{e^{\htmlData{part=4}{-x^2}}}\htmlData{part=5}{\,dx}} \htmlData{part=6}{=} \htmlData{part=7}{\frac{\htmlData{part=8}{\sqrt{\pi}}}{\htmlData{part=10}{2}}}")
        );
        assert_eq!(a.kinds, [Part, Link, Part, Link, Part, Part, Part, Part, Part, Link, Part]);
    }

    #[test]
    fn tipos_de_frase() {
        let a = annotate(r"\frac{a}{b}", &[r"\frac{", ""]);
        assert_eq!((a.latex, a.kinds), (None, vec![Link, Link]));
        // `\sqrt` sin su argumento no es autocontenido.
        let a = annotate(r"\sqrt{x} + 1", &[r"\sqrt"]);
        assert_eq!((a.latex, a.kinds), (None, vec![Lost]));
    }

    #[test]
    fn no_parte_comandos() {
        assert_eq!(ann(r"\int_0^1 f", &["in"]), None);
    }

    #[test]
    fn ignora_fragmentos_desbalanceados() {
        assert_eq!(ann(r"\frac{a}{b}", &["a}{b"]), None);
    }

    #[test]
    fn argumentos_sueltos() {
        // Un token tras `^` se envuelve con llaves para que siga siendo el exponente.
        assert_eq!(ann("x^2 + y", &["2"]).as_deref(), Some(r"x^{\htmlData{part=0}{2}} + y"));
        assert_eq!(ann(r"\frac12", &["1"]).as_deref(), Some(r"\frac{\htmlData{part=0}{1}}2"));
        // Varios tokens tras `_` sin llaves cambiarían el dibujo: no se marcan.
        assert_eq!(ann("x_1 y", &["1 y"]), None);
    }

    /// Lectura real del gradiente: envolver el `[` de `\left[` hacía que KaTeX rechazara la
    /// fórmula entera y no se resaltaba ninguna parte. La apertura marca el vector completo y el
    /// cierre queda como enlace.
    #[test]
    fn corchetes_de_left_right() {
        let latex = r"\nabla f(x) = \left[\frac{\partial f}{\partial x_1}(x), \frac{\partial f}{\partial x_2}(x)\right]^T";
        let texs = [
            r"\nabla f(x)", "=", r"\left[", r"\partial f", "", r"\partial x_1", "(x)", ",", r"\partial f", "",
            r"\partial x_2", "(x)", r"\right]", "^T",
        ];
        let a = annotate(latex, &texs);
        assert_eq!(
            a.latex.as_deref(),
            Some(r"\htmlData{part=0}{\nabla f(x)} \htmlData{part=1}{=} \htmlData{part=2}{\left[\frac{\htmlData{part=3}{\partial f}}{\htmlData{part=5}{\partial x_1}}\htmlData{part=6}{(x)}\htmlData{part=7}{,} \frac{\htmlData{part=8}{\partial f}}{\htmlData{part=10}{\partial x_2}}\htmlData{part=11}{(x)}\right]^{\htmlData{part=13}{T}}}")
        );
        assert_eq!(a.kinds, [Part, Part, Part, Part, Link, Part, Part, Part, Part, Link, Part, Part, Link, Part]);
        assert_eq!(a.lost(), 0);
        // Lecturas reales que se descartaban por «omitir contenido»: `\left`, `[`, `&`… no se leen.
        let says = owned(&[
            "nabla f de x", "igual", "el vector", "parcial de f", "entre", "parcial de x sub uno", "de x", "coma",
            "parcial de f", "entre", "parcial de x sub dos", "de x", "cierra el vector", "transpuesta",
        ]);
        assert_eq!(a.omissions(&says), 0);
        let norm = annotate(
            r"\left\| \mathbf{w} \right\|_2 = \sqrt{w_1^2 + w_2^2}",
            &[r"\left\| \mathbf{w} \right\|_2", r"\mathbf{w}", "=", r"\sqrt{w_1^2 + w_2^2}", "w_1^2", "+", "w_2^2"],
        );
        let says = owned(&["la norma dos de", "w", "igual a", "la raíz cuadrada de", "w sub uno al cuadrado", "más", "w sub dos al cuadrado"]);
        assert_eq!((norm.lost(), norm.omissions(&says)), (0, 0));
        // Tal como lo devolvió el modelo (`[` y `]` sin `\left`): el mismo resultado.
        let texs = texs.map(|x| x.trim_start_matches(r"\left").trim_start_matches(r"\right"));
        assert_eq!(annotate(latex, &texs).kinds, a.kinds);
        // Sin `\left` el corchete es un símbolo más y sí se marca.
        assert_eq!(
            ann(r"\nabla f(x) = [2x_1, 2x_2]^T", &[r"\nabla f(x)", "=", "[", "2x_1", ",", "2x_2", "]", "^T"]).as_deref(),
            Some(r"\htmlData{part=0}{\nabla f(x)} \htmlData{part=1}{=} \htmlData{part=2}{[}\htmlData{part=3}{2x_1}\htmlData{part=4}{,} \htmlData{part=5}{2x_2}\htmlData{part=6}{]^{\htmlData{part=7}{T}}}")
        );
        // `\bigl(` también toma un delimitador; `\bigcup` no.
        assert_eq!(annotate(r"\bigl( a + b \bigr)", &["(", "a + b", ")"]).kinds, [Link, Part, Link]);
        assert!(!takes_delimiter(r"\bigcup"));
    }

    #[test]
    fn matrices() {
        let latex = r"A = \begin{bmatrix} a & b \\ c & d \end{bmatrix}";
        // La `a` de `bmatrix` no es la variable `a`.
        assert_eq!(
            ann(latex, &["A", "=", "a", "b", "c", "d"]).as_deref(),
            Some(r"\htmlData{part=0}{A} \htmlData{part=1}{=} \begin{bmatrix} \htmlData{part=2}{a} & \htmlData{part=3}{b} \\ \htmlData{part=4}{c} & \htmlData{part=5}{d} \end{bmatrix}")
        );
        // «La matriz» marca la matriz completa (también con sólo `\begin{bmatrix}`); una fila
        // suelta con `&` no se puede marcar.
        let whole = Some(r"A = \htmlData{part=0}{\begin{bmatrix} a & b \\ c & d \end{bmatrix}}");
        assert_eq!(ann(latex, &[r"\begin{bmatrix} a & b \\ c & d \end{bmatrix}"]).as_deref(), whole);
        assert_eq!(ann(latex, &[r"\begin{bmatrix}"]).as_deref(), whole);
        assert_eq!(ann(latex, &["a & b"]), None);
        // Lectura real: los `&` y `\\` pegados a los números y el cierre de la matriz.
        let latex = r"A = \begin{bmatrix} 1 & 2 \\ 3 & 4 \end{bmatrix}";
        let a = annotate(latex, &["A", "=", r"\begin{bmatrix}", "1", "& 2", r"\\ 3", "& 4", r"\end{bmatrix}"]);
        assert_eq!(
            a.latex.as_deref(),
            Some(r"\htmlData{part=0}{A} \htmlData{part=1}{=} \htmlData{part=2}{\begin{bmatrix} \htmlData{part=3}{1} & \htmlData{part=4}{2} \\ \htmlData{part=5}{3} & \htmlData{part=6}{4} \end{bmatrix}}")
        );
        assert_eq!(a.kinds, [Part, Part, Part, Part, Part, Part, Part, Link]);
        let says = owned(&["A", "igual", "matriz de dos por dos", "uno", "dos", "tres", "cuatro", "fin de la matriz"]);
        assert_eq!(a.omissions(&says), 0);
    }

    #[test]
    fn formulas_simples_no_se_dividen() {
        assert!(!needs_parts("v_1"));
        assert!(!needs_parts(r"\mathbf{v}_2"));
        assert!(needs_parts("v_1^T v_2"));
        assert!(needs_parts(r"s_\theta(v_1, v_2) = 0.6682"));
    }
}
