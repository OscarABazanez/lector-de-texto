# Lector

Lector de pantalla para Windows y macOS.

1. Pulsas un atajo (**Ctrl+Alt+L**) y seleccionas una zona de la pantalla.
2. Claude Haiku 5.5 (API de Anthropic) transcribe el texto y las fórmulas.
3. La app lo lee en voz alta y resalta en una ventana la palabra o la fórmula que está diciendo.

Las fórmulas se leen como lo haría un profesor: *"ese sub theta de v sub uno coma v sub dos es igual a v sub uno transpuesta por v sub dos entre la norma dos de v sub uno…"*.

## Cómo funciona

```
Atajo → captura del monitor (xcap) → overlay de selección → recorte PNG
  → Etapa 1, Claude Haiku 5.5 con visión y streaming: Markdown + LaTeX
      → parser incremental → segmentos de texto y fórmulas → ventana (KaTeX)
  → detección de idioma es/en
  → Etapa 2, LLM solo texto, en paralelo y con caché: cada fórmula → lectura hablada
  → fragmentos por oración → voz (del sistema o Edge) → audio + marcas por palabra
  → reproductor (rodio) → resaltado sincronizado
```

- La primera oración empieza a sonar mientras el modelo sigue transcribiendo el resto.
- El modo rápido usa `thinking: {type: "disabled"}` con effort `low`: en las pruebas tardó entre 0.6 y 1.2 s al primer token y acertó todos los subíndices. El *modo preciso* usa thinking adaptativo.
- Si no hay internet o la API key falla, se usa el OCR de Windows como respaldo (sin fórmulas).
- Costo aproximado: ≈ $0.0003 por lectura ($0.10 / $0.50 por millón de tokens de entrada / salida).
- Si las voces de Edge fallan (no hay internet), se cambia solo a las voces del sistema.

## Requisitos

- Windows 10/11 con WebView2 (ya viene en Windows 11), o macOS 11 o posterior.
- Rust (toolchain MSVC) y Visual Studio Build Tools con C++.
- Una API key de Anthropic (console.anthropic.com). Se guarda desde **Ajustes** en el Administrador de credenciales de Windows, nunca en `config.toml`. También se acepta la variable de entorno `ANTHROPIC_API_KEY`.
- Las capturas se envían a Anthropic para transcribirlas.

## Compilar y ejecutar

```powershell
cd src-tauri
cargo test                      # pruebas unitarias
cargo run                       # la app (queda en la bandeja del sistema)
cargo run --example pipeline -- ..\tests\fixtures\coseno.png          # prueba sin interfaz
cargo run --example pipeline -- --edge ..\tests\fixtures\coseno.png   # con voz de Edge
cargo run --example pipeline -- --text "Sea $v_1$ un vector."          # sin transcribir imagen
cargo run --example tts_marks -- "Similitud del coseno."            # marcas de tiempo de cada voz
```

## Instaladores

| Sistema | Cómo | Resultado |
|---|---|---|
| Windows | `powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1` | `src-tauri\target\release\bundle\nsis\Lector_0.1.0_x64-setup.exe` |
| macOS (en una Mac) | `bash scripts/build-macos.sh` | `src-tauri/target/universal-apple-darwin/release/bundle/dmg/Lector_0.1.0_universal.dmg` |
| Los dos, sin Mac | Subir el código a GitHub y lanzar **Actions → Instaladores** (`.github/workflows/instaladores.yml`) | Los dos archivos como *artifacts* |

El instalador de macOS sólo se puede generar en macOS, porque Apple no permite compilar sus apps desde Windows. Los instaladores no van firmados:
- en Windows, SmartScreen pide «Más información → Ejecutar de todas formas»;
- en macOS, la primera vez hay que abrir la app con clic derecho → Abrir.

## Configuración

La configuración vive en `%APPDATA%\lector\config.toml` y también se edita desde **Ajustes** en el icono de la bandeja.

| Clave | Por defecto |
|---|---|
| `hotkey` | `Ctrl+Alt+L` |
| `llm.model` | `claude-haiku-5-5` |
| `llm.precise_mode` | `false` |
| `tts.engine` | `system` (voces del sistema) o `edge` |
| `tts.rate` | `1.0` |
| `tts.system_es` / `system_en` | Windows: `Microsoft Sabina` / `Microsoft Zira`. macOS: vacío, que usa la voz del sistema |
| `tts.edge_es` / `edge_en` | `es-MX-DaliaNeural` / `en-US-AriaNeural` |
| `ui.default_language` | `es` |

## Uso del lector

| Tecla / control | Acción |
|---|---|
| Espacio | Pausa o continuar |
| ← / → (botones «2») | Retroceder / adelantar 2 palabras |
| Ctrl+← / Ctrl+→ (⌘ en Mac) | Oración anterior o siguiente |
| R | Repetir la oración |
| Clic en una palabra | Leer desde ahí |
| Esc | Cerrar |

## Estructura

| Ruta | Contenido |
|---|---|
| `src-tauri/src/app.rs` | Tauri: bandeja, atajo, ventanas y comandos |
| `src-tauri/src/pipeline.rs` | Orquestación (productor y alimentador) y cancelación |
| `src-tauri/src/doc/` | Parser incremental, fragmentos y guion con mapa de palabras |
| `src-tauri/src/llm/` | Cliente de la API de Claude (HTTP + SSE), prompts, etapa 1 y etapa 2 |
| `src-tauri/src/secrets.rs` | API key en el almacén de credenciales del sistema |
| `src-tauri/src/tts/` | Motores `winrt` (Windows), `say` (macOS) y `edge` |
| `src-tauri/src/player.rs` | Audio y tiempos de resaltado |
| `src-tauri/src/capture.rs`, `ocr.rs` | Captura de pantalla y OCR de respaldo |
| `ui/` | HTML, CSS y JS sin bundler (overlay, lector, ajustes) y KaTeX |

## macOS

- **Voz:** la del sistema, mediante el comando `say`. Usa la que elegiste en Ajustes del Sistema → Accesibilidad → Contenido leído; en Ajustes de Lector puedes elegir otra. `say` no informa cuándo empieza cada palabra, así que el resaltado se **estima** con la duración real de la voz.
- **Permisos:** la primera captura pide permiso de **Grabación de pantalla** (Ajustes del Sistema → Privacidad y seguridad). Después de darlo hay que reabrir Lector.
- **Atajo:** Control+Opción+L.
- **API key:** se guarda en el Llavero de macOS.
- **Sin internet:** no hay OCR de respaldo (el de Windows no existe en Mac); se muestra un aviso.
- Lo que es igual en los dos sistemas: Edge, el audio (rodio), la captura (xcap), el atajo y la interfaz.
- **No está probado en una Mac real.** El código específico de macOS se compila y sus pruebas corren en Windows, pero la llamada a `say` y los permisos sólo se pueden comprobar en una Mac.
