//! Prompts probados con Gemma 4 E4B y Claude Haiku 5.5 (ver el plan del proyecto).

use serde_json::{json, Value};

use crate::lang::Lang;

/// Pedir explícitamente los subíndices pequeños evita que el modo rápido los omita
/// (p. ej. el ₂ de ‖v‖₂) sin costo de tiempo.
pub const TRANSCRIBE: &str = "Transcribe exactamente el texto de la imagen en Markdown. \
Escribe toda expresión matemática (incluidas variables sueltas con subíndices) en LaTeX entre \
$...$ (en línea) o $$...$$ (bloque). Copia cada símbolo tal como aparece: revisa con cuidado los \
subíndices y superíndices pequeños (por ejemplo \\|x\\|_2, x_i^2, v^T) y no los omitas. \
Escribe las variables en cursiva normal (v_1, no \\mathbf{v}_1); usa \\mathbf solo si la letra se ve \
en negritas. Conserva el idioma original del texto. No agregues nada más.";

const MATH_ES: &str = "Eres un profesor de matemáticas leyendo en voz alta en español de México. \
Convierte la expresión LaTeX en palabras, de forma natural y fiel (no agregues nada que no esté en la expresión). \
No nombres los paréntesis ni la puntuación: léelos como lo haría una persona. Ejemplos:
v_1 → v sub uno
f(x, y) → f de x y y
\\mathbf{v}_1 → v sub uno
v_1^T v_2 → v sub uno transpuesta por v sub dos
\\frac{a}{b} → a entre b
\\|v\\| → la norma de v
\\|v\\|_2 → la norma dos de v
x^2 → x al cuadrado
\\theta → theta
0.6682 → cero punto seis seis ocho dos
Responde SOLO con la lectura, sin comillas ni explicaciones.";

const MATH_EN: &str = "You are a math teacher reading aloud in English. \
Convert the LaTeX expression into words, naturally and faithfully (do not add anything that is not in the expression). \
Do not name parentheses or punctuation: read them the way a person would. Examples:
v_1 → v sub one
f(x, y) → f of x and y
\\mathbf{v}_1 → v sub one
v_1^T v_2 → v sub one transpose times v sub two
\\frac{a}{b} → a over b
\\|v\\| → the norm of v
\\|v\\|_2 → the two-norm of v
x^2 → x squared
\\theta → theta
0.6682 → zero point six six eight two
Respond ONLY with the reading, no quotes or explanations.";

/// Igual que `MATH_*`, pero dividida en frases con el fragmento de LaTeX que lee cada una.
/// Pedir fragmentos completos y conectores aparte redujo los fragmentos imposibles de marcar;
/// pedir leer el contenido de raíces evita que lo omita (pasaba 8 de 8 veces) y prohibir la
/// fracción completa como fragmento evita marcarla entera al leer el numerador (120 lecturas sin fallos).
const MATH_PARTS_ES: &str = "Eres un profesor de matemáticas leyendo en voz alta en español de México. Convierte la expresión LaTeX en una lectura natural y fiel (no agregues nada que no esté en la expresión), dividida en frases cortas en el orden en que se dicen. Para cada frase da:
- say: las palabras que se dicen. No nombres los paréntesis ni la puntuación.
- tex: el fragmento EXACTO del LaTeX de entrada que esa frase lee, copiado carácter por carácter. Debe ser un fragmento completo: un término, un símbolo o un grupo entero (por ejemplo \"v_1^T\", \"\\|v_1\\|_2\", \"=\"), nunca la mitad de un comando ni de unas llaves.
- Lee TODA la expresión, sin omitir nada. Si una frase lee un comando que contiene otra expresión (una raíz, una norma, un exponente sobre un paréntesis), esa frase usa como tex el grupo completo y después lees su contenido en frases propias, cada una con su fragmento.
- Nunca uses como tex una fracción completa (\\frac{…}{…}): lee el numerador en su propia frase con su fragmento, después \"entre\" con tex \"\" y después el denominador en su propia frase con el suyo. No omitas el denominador.
- Las palabras de enlace que no corresponden a un símbolo visible, como \"entre\" en una fracción o \"por\" entre dos factores juntos, van solas en su propia frase con tex \"\".
Ejemplo: para \\frac{a+b}{c} las frases son {\"say\": \"a más b\", \"tex\": \"a+b\"}, {\"say\": \"entre\", \"tex\": \"\"}, {\"say\": \"c\", \"tex\": \"c\"}.
Ejemplo: para \\sqrt{a^2 + b^2} las frases son {\"say\": \"la raíz cuadrada de\", \"tex\": \"\\sqrt{a^2 + b^2}\"}, {\"say\": \"a al cuadrado\", \"tex\": \"a^2\"}, {\"say\": \"más\", \"tex\": \"+\"}, {\"say\": \"b al cuadrado\", \"tex\": \"b^2\"}.
Ejemplos de lectura:
v_1 → v sub uno
\\mathbf{v}_1 → v sub uno
v_1^T v_2 → v sub uno transpuesta por v sub dos
\\frac{a}{b} → a entre b
\\|v\\| → la norma de v
\\|v\\|_2 → la norma dos de v
x^2 → x al cuadrado
0.6682 → cero punto seis seis ocho dos";

const MATH_PARTS_EN: &str = "You are a math teacher reading aloud in English. Convert the LaTeX expression into a natural, faithful reading (do not add anything that is not in the expression), split into short phrases in the order they are spoken. For each phrase give:
- say: the words spoken. Do not name parentheses or punctuation.
- tex: the EXACT fragment of the input LaTeX that the phrase reads, copied character by character. It must be a complete fragment: a term, a symbol or a whole group (for example \"v_1^T\", \"\\|v_1\\|_2\", \"=\"), never half of a command or of a pair of braces.
- Read the WHOLE expression, leaving nothing out. If a phrase reads a command that contains another expression (a root, a norm, an exponent over a parenthesis), that phrase uses the whole group as tex and then you read its content in phrases of its own, each with its fragment.
- Never use a whole fraction (\\frac{…}{…}) as tex: read the numerator in its own phrase with its fragment, then \"over\" with tex \"\", then the denominator in its own phrase with its own. Do not leave out the denominator.
- Linking words that do not match a visible symbol, like \"over\" in a fraction or \"times\" between two adjacent factors, go alone in their own phrase with tex \"\".
Example: for \\frac{a+b}{c} the phrases are {\"say\": \"a plus b\", \"tex\": \"a+b\"}, {\"say\": \"over\", \"tex\": \"\"}, {\"say\": \"c\", \"tex\": \"c\"}.
Example: for \\sqrt{a^2 + b^2} the phrases are {\"say\": \"the square root of\", \"tex\": \"\\sqrt{a^2 + b^2}\"}, {\"say\": \"a squared\", \"tex\": \"a^2\"}, {\"say\": \"plus\", \"tex\": \"+\"}, {\"say\": \"b squared\", \"tex\": \"b^2\"}.
Reading examples:
v_1 → v sub one
\\mathbf{v}_1 → v sub one
v_1^T v_2 → v sub one transpose times v sub two
\\frac{a}{b} → a over b
\\|v\\| → the norm of v
\\|v\\|_2 → the two-norm of v
x^2 → x squared
0.6682 → zero point six six eight two";

pub fn math_parts(lang: Lang) -> &'static str {
    match lang {
        Lang::Es => MATH_PARTS_ES,
        Lang::En => MATH_PARTS_EN,
    }
}

/// Schema de *structured outputs* para `MATH_PARTS_*`. El orden de las claves importa: con las
/// claves ordenadas alfabéticamente (lo que hace serde_json sin `preserve_order`) el modelo omitía
/// contenido 8 de 10 veces; con `type` primero, 0 de 10.
pub fn pieces_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "pieces": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {"say": {"type": "string"}, "tex": {"type": "string"}},
                    "required": ["say", "tex"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["pieces"],
        "additionalProperties": false,
    })
}

pub fn math(lang: Lang) -> &'static str {
    match lang {
        Lang::Es => MATH_ES,
        Lang::En => MATH_EN,
    }
}
