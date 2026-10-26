//! Hilo dedicado (COM multihilo) donde se ejecutan las llamadas bloqueantes a WinRT.

use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use anyhow::anyhow;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

type Job = Box<dyn FnOnce() + Send>;

fn sender() -> &'static Sender<Job> {
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("winrt".into())
            .spawn(move || {
                // SAFETY: inicializa COM una sola vez para este hilo; nunca se desinicializa.
                let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                for job in rx {
                    job();
                }
            })
            .expect("hilo WinRT");
        tx
    })
}

/// Ejecuta `f` en el hilo WinRT y espera su resultado sin bloquear el runtime async.
pub async fn run<T, F>(f: F) -> anyhow::Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    sender()
        .send(Box::new(move || {
            let _ = tx.send(f());
        }))
        .map_err(|_| anyhow!("el hilo WinRT se detuvo"))?;
    rx.await.map_err(|_| anyhow!("el hilo WinRT no respondió"))
}
