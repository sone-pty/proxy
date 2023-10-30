#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]

use std::sync::LazyLock;

use client::Client;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use vnpkt::tokio_ext::registry::Registry;
use vnsvrbase::{process::hook_terminate_signal, tokio_ext::tcp_link::TcpLink};

mod client;

static REGISTRY: LazyLock<Registry<Client>> = LazyLock::new(Registry::new);

fn main() {
    let (quit_tx, mut quit_rx) = tokio::sync::watch::channel(false);
    hook_terminate_signal(Some(move || {
        let _ = quit_tx.send(true);
    }));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_time()
        .enable_io()
        .build()
        .unwrap();

    let wrt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_time()
        .enable_io()
        .build()
        .unwrap();

    rt.block_on(async {
        tokio::spawn(main_loop(wrt.handle().clone()));
        quit_rx.changed().await
    })
    .unwrap();
}

async fn main_loop(wrt: tokio::runtime::Handle) -> std::io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:60010").await?;
    let handle = tokio::runtime::Handle::current();
    let (sender, mut recv) = mpsc::channel::<TcpStream>(50000);

    let sender_clone = sender.clone();
    tokio::spawn(async move {
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                let _ = sender_clone.send(stream).await;
            }
        }
    });

    while let Some(stream) = recv.recv().await {
        TcpLink::attach(stream, &wrt, &handle, Client::receiving);
    }
    Ok(())
}
