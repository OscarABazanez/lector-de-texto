//! Reproductor en un hilo propio (rodio) que avisa cuándo empieza cada palabra.

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::Duration;

use rodio::Source;

use crate::doc::Target;

/// Audio de un fragmento y en qué milisegundo se resalta cada palabra/fórmula.
pub struct PlayItem {
    pub tag: u64,
    pub audio: Vec<u8>,
    pub marks: Vec<(u32, Target)>,
    /// Fin de la voz; se corta ahí (más un margen) el silencio que dejan los motores.
    pub end_ms: Option<u32>,
}

#[derive(Clone, Debug)]
pub enum PlayerEvent {
    Started { tag: u64 },
    Highlight { tag: u64, target: Target },
    Finished { tag: u64 },
    Paused(bool),
    Error(String),
}

enum Cmd {
    Enqueue(PlayItem),
    TogglePause,
    Clear,
}

pub struct Player {
    tx: Sender<Cmd>,
}

struct Current {
    tag: u64,
    marks: Vec<(u32, Target)>,
    next: usize,
}

const TICK: Duration = Duration::from_millis(15);
/// Pausa natural que se conserva después de la última palabra.
const TAIL_MS: u64 = 280;

impl Player {
    pub fn new(on_event: impl Fn(PlayerEvent) + Send + 'static) -> Player {
        let (tx, rx) = channel::<Cmd>();
        std::thread::Builder::new()
            .name("player".into())
            .spawn(move || {
                let mut sink = match rodio::DeviceSinkBuilder::open_default_sink() {
                    Ok(s) => s,
                    Err(e) => {
                        on_event(PlayerEvent::Error(format!("no hay salida de audio: {e}")));
                        return;
                    }
                };
                sink.log_on_drop(false);
                let mut player = rodio::Player::connect_new(sink.mixer());
                let mut queue: VecDeque<PlayItem> = VecDeque::new();
                let mut current: Option<Current> = None;
                let mut paused = false;

                loop {
                    let busy = current.is_some() || !queue.is_empty();
                    let cmd = if busy {
                        rx.recv_timeout(TICK)
                    } else {
                        rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match cmd {
                        Ok(Cmd::Enqueue(item)) => queue.push_back(item),
                        Ok(Cmd::TogglePause) => {
                            if current.is_some() || !queue.is_empty() {
                                paused = !paused;
                                if paused {
                                    player.pause();
                                } else {
                                    player.play();
                                }
                                on_event(PlayerEvent::Paused(paused));
                            }
                        }
                        Ok(Cmd::Clear) => {
                            queue.clear();
                            if current.take().is_some() {
                                // Al soltar el Player anterior se corta su sonido.
                                player = rodio::Player::connect_new(sink.mixer());
                            }
                            if paused {
                                paused = false;
                                on_event(PlayerEvent::Paused(false));
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }

                    if let Some(cur) = current.as_mut() {
                        let pos = player.get_pos().as_millis() as u32;
                        while cur.next < cur.marks.len() && cur.marks[cur.next].0 <= pos {
                            on_event(PlayerEvent::Highlight { tag: cur.tag, target: cur.marks[cur.next].1 });
                            cur.next += 1;
                        }
                        if player.empty() {
                            on_event(PlayerEvent::Finished { tag: cur.tag });
                            current = None;
                        }
                    }

                    if current.is_none() && !paused {
                        while let Some(item) = queue.pop_front() {
                            match rodio::Decoder::new(Cursor::new(item.audio)) {
                                Ok(src) => {
                                    // Un Player nuevo por fragmento: así `get_pos` arranca en cero.
                                    player = rodio::Player::connect_new(sink.mixer());
                                    match item.end_ms {
                                        Some(end) => player.append(
                                            src.take_duration(Duration::from_millis(u64::from(end) + TAIL_MS)),
                                        ),
                                        None => player.append(src),
                                    }
                                    on_event(PlayerEvent::Started { tag: item.tag });
                                    current = Some(Current { tag: item.tag, marks: item.marks, next: 0 });
                                    break;
                                }
                                Err(e) => {
                                    on_event(PlayerEvent::Error(format!("audio inválido: {e}")));
                                    on_event(PlayerEvent::Finished { tag: item.tag });
                                }
                            }
                        }
                    }
                }
            })
            .expect("hilo de audio");
        Player { tx }
    }

    pub fn enqueue(&self, item: PlayItem) {
        let _ = self.tx.send(Cmd::Enqueue(item));
    }

    pub fn toggle_pause(&self) {
        let _ = self.tx.send(Cmd::TogglePause);
    }

    /// Detiene lo que suena y vacía la cola.
    pub fn clear(&self) {
        let _ = self.tx.send(Cmd::Clear);
    }
}
