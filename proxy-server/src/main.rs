#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]
#![feature(async_closure)]
#![feature(sync_unsafe_cell)]

use std::{sync::LazyLock, time::Duration};

use agent::Agent;
use clap::Parser;
use client::Client;
use conn::{ClientConns, Conns};
use dashmap::DashMap;
use protocol::{get_id, is_client, BOUDARY};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{
        mpsc,
        watch::{channel, Receiver, Sender},
    },
};
use vnpkt::tokio_ext::{io::AsyncReadExt, registry::Registry};
use vnsvrbase::{process::hook_terminate_signal, tokio_ext::tcp_link::TcpLink};

mod agent;
mod client;
mod conn;

static REGISTRY_CLIENT: LazyLock<Registry<Client>> = LazyLock::new(Registry::new);
static REGISTRY_AGENT: LazyLock<Registry<Agent>> = LazyLock::new(Registry::new);
static CLIENTS: LazyLock<ClientConns> = LazyLock::new(|| ClientConns::new());
static CONNS: LazyLock<DashMap<u32, Conns>> = LazyLock::new(|| DashMap::new());
static CHANNEL: LazyLock<(
    Sender<Option<vnsvrbase::tokio_ext::tcp_link::Handle>>,
    Receiver<Option<vnsvrbase::tokio_ext::tcp_link::Handle>>,
)> = LazyLock::new(|| channel(None));

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

                    handle.spawn(async move {
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                            _ = async move {
                                use tokio::io::AsyncReadExt;
                                if let Ok(cid) = stream.read_u32().await {
                                    match CONNS.get(&cid) {
                                        Some(conns) => {
                                            if let Ok(data) = stream.read_compressed_u64().await {
                                                let id = get_id(&data);

                                                if is_client(&data) {
                                                    tokio::spawn(async move {
                                                        match conns.get_rx(id) {
                                                            Some(rx) => {
                                                                match rx.await {
                                                                    Ok(mut peer) => {
                                                                        conns.remove(id);
                                                                        println!("Conn.{} Begin", id);
                                                                        match tokio::io::copy_bidirectional(&mut peer, &mut stream).await {
                                                                            Ok(_) => println!("Conn.{} Disconnected", id),
                                                                            Err(e) => println!("Conn.{} Error: {}", id, e)
                                                                        }
                                                                    },
                                                                    _ => {}
                                                                }
                                                            },
                                                            _ => {}
                                                        }
                                                    });
                                                } else {
                                                    match conns.get_sx(id) {
                                                        Some(sx) => {
                                                            let _ = sx.send(stream);
                                                        },
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        },
                                        _ => {}
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
    let mut pid = link.read.read_compressed_u64().await?;
    if pid <= u32::MAX as _ {
        if (pid as u32) < BOUDARY {
            let mut client = Client::new(link.handle().clone(), CHANNEL.1.clone());
            let register_client = &*REGISTRY_CLIENT;

            loop {
                if pid <= u32::MAX as _ {
                    if let Some(item) = register_client.query(pid as u32) {
                        let r = item.recv(&mut link.read).await?;
                        r.proc(&mut client).await?;
                        pid = link.read.read_compressed_u64().await?;
                        continue;
                    }
                }
                return Err(std::io::ErrorKind::InvalidData.into());
            }
        } else {
            let mut agent = Agent::new(link.handle().clone());
            let register_agent = &*REGISTRY_AGENT;

            loop {
                if pid <= u32::MAX as _ {
                    if let Some(item) = register_agent.query(pid as u32) {
                        let r = item.recv(&mut link.read).await?;
                        r.proc(&mut agent).await?;
                        pid = link.read.read_compressed_u64().await?;
                        continue;
                    }
                }
                return Err(std::io::ErrorKind::InvalidData.into());
            }
        }
    }
    Err(std::io::ErrorKind::InvalidData.into())
}
