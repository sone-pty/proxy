#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]
#![feature(async_closure)]
#![feature(sync_unsafe_cell)]

use std::{sync::LazyLock, time::Duration};

use agent::Agent;
use clap::Parser;
use client::Client;
use conn::{ClientConns, Conns};
use protocol::{get_id, is_client, BOUDARY};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch::channel},
};
use vnpkt::tokio_ext::{io::AsyncReadExt, registry::Registry};
use vnsvrbase::{process::hook_terminate_signal, tokio_ext::tcp_link::TcpLink};

mod agent;
mod client;
mod conn;

static REGISTRY_CLIENT: LazyLock<Registry<Client>> = LazyLock::new(Registry::new);
static REGISTRY_AGENT: LazyLock<Registry<Agent>> = LazyLock::new(Registry::new);
static CLIENTS: LazyLock<ClientConns> = LazyLock::new(|| ClientConns::new());
static CONNS: LazyLock<Conns> = LazyLock::new(|| Conns::new());

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
                TcpLink::attach(stream, &wrt, &handle, receiving);
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
                                    let conns = &*CONNS;

                                    if is_client(&data) {
                                        conns.get_rx(id).map(async move |rx| {
                                            unsafe {
                                                let guard = &mut *rx.get();
                                                match guard.recv().await {
                                                    Some(mut peer) => {
                                                        CONNS.remove(id);
                                                        let _ = tokio::io::copy_bidirectional(&mut peer, &mut stream).await;
                                                    },
                                                    _ => {}
                                                }
                                            }
                                        });
                                    } else {
                                        conns.get_sx(id).map(async move |sx| {
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

async fn receiving(link: &mut TcpLink) -> std::io::Result<()> {
    let (sx, rx) = channel(None);
    let mut agent = Agent::new(link.handle().clone(), sx);
    let register_agent = &*REGISTRY_AGENT;

    loop {
        let pid = link.read.read_compressed_u64().await?;
        if pid <= u32::MAX as _ {
            if (pid as u32) < BOUDARY {
                let mut client = Client::new(link.handle().clone(), rx.clone());
                let register = &*REGISTRY_CLIENT;
                if let Some(item) = register.query(pid as u32) {
                    let r = item.recv(&mut link.read).await?;
                    r.proc(&mut client).await?;
                    continue;
                }
            } else if let Some(item) = register_agent.query(pid as u32) {
                let r = item.recv(&mut link.read).await?;
                r.proc(&mut agent).await?;
                continue;
            }
        }
        break Err(std::io::ErrorKind::InvalidData.into());
    }
}
