#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]
#![feature(async_closure)]
#![feature(sync_unsafe_cell)]

use std::{sync::LazyLock, time::Duration};

use clap::Parser;
use client::Client;
use conn::Conns;
use protocol::{get_id, is_client};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use vnpkt::tokio_ext::{io::AsyncReadExt, registry::Registry};
use vnsvrbase::{process::hook_terminate_signal, tokio_ext::tcp_link::TcpLink};

mod client;
mod conn;

static REGISTRY: LazyLock<Registry<Client>> = LazyLock::new(Registry::new);
static ROUTES: LazyLock<Conns> = LazyLock::new(|| Conns::new());

#[derive(Parser)]
struct Args {
    main_port: u16,
    conn_port: u16,
}

fn main() {
    let args = Args::parse();
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
        tokio::spawn(main_loop(wrt.handle().clone(), args));
        quit_rx.changed().await
    })
    .unwrap();
}

async fn main_loop(wrt: tokio::runtime::Handle, args: Args) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", args.main_port)).await?;
    let conn_listener = TcpListener::bind(("0.0.0.0", args.conn_port)).await?;
    let handle = tokio::runtime::Handle::current();

    tokio::select! {
        _ = async {
            let (sender, mut recv) = mpsc::channel::<TcpStream>(1000);
            let sender_clone = sender.clone();
            tokio::spawn(async move {
                loop {
                    if let Ok((stream, _)) = listener.accept().await {
                        let _ = sender_clone.send(stream).await;
                    }
                }
            });

            while let Some(stream) = recv.recv().await {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_linger(None);
                TcpLink::attach(stream, &wrt, &handle, Client::receiving);
            }
        } => {}
        _ = async {
            loop {
                if let Ok((mut stream, _)) = conn_listener.accept().await {
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_linger(None);

                    handle.spawn(async {
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                            _ = async {
                                if let Ok(data) = stream.read_compressed_u64().await {
                                    let id = get_id(&data);
                                    let routes = &*ROUTES;

                                    if is_client(&data) {
                                        routes.get_rx(id).map(async move |rx| {
                                            unsafe {
                                                let guard = &mut *rx.get();
                                                match guard.recv().await {
                                                    Some(mut peer) => {
                                                        let _ = tokio::io::copy_bidirectional(&mut peer, &mut stream).await;
                                                    },
                                                    _ => {}
                                                }
                                            }
                                        });
                                    } else {
                                        routes.get_sx(id).map(async move |sx| {
                                            let _ = sx.send(stream).await;
                                        });
                                    }
                                }
                            } => {}
                        }
                    });
                }
            }
        } => {}
    }
    Ok(())
}
