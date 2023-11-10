#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]
#![feature(async_closure)]
#![feature(sync_unsafe_cell)]

use std::{
    collections::HashMap,
    sync::{LazyLock, RwLock},
};

use agent::AgentHandler;
use clap::Parser;
use client::ClientHandler;
use conn::Agent;
use tokio::net::{TcpListener, TcpStream};
use vnpkt::tokio_ext::{io::AsyncReadExt, registry::Registry};
use vnsvrbase::{process::hook_terminate_signal, tokio_ext::tcp_link::TcpLink};

mod agent;
mod client;
mod conn;

static REGISTRY_CLIENT: LazyLock<Registry<ClientHandler>> = LazyLock::new(Registry::new);
static REGISTRY_AGENT: LazyLock<Registry<AgentHandler>> = LazyLock::new(Registry::new);
static AGENTS: LazyLock<RwLock<HashMap<u32, Agent>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

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
        .worker_threads(3)
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
            let (sender, mut recv) = tokio::sync::mpsc::channel::<TcpStream>(1000);
            let sender_clone = sender.clone();
            tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            let _ = sender_clone.send(stream).await;
                        }
                        Err(e) => {
                            println!("main listener accept failed: {}", e);
                        }
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

                    use tokio::io::AsyncReadExt;
                    if let (Ok(agent_id), Ok(cid), Ok(data)) = async {
                        (stream.read_u32().await, stream.read_u32().await, stream.read_compressed_u64().await)
                    }.await {
                        let sid = protocol::get_id(&data);

                        if protocol::is_client(&data) {
                            #[allow(unused_assignments)]
                            let mut rx_wrap = None;
                            {
                                let agents = AGENTS.read().unwrap();
                                rx_wrap = agents.get(&agent_id).map_or(None, |agent| agent.get_conn_rx(cid, sid));
                            }

                            if rx_wrap.is_some() {
                                tokio::spawn(async move {
                                    match rx_wrap.unwrap().await {
                                        Ok(mut peer) => {
                                            println!("In the proxy.{}, conn.{} begin", agent_id, sid);
                                            // remove conn
                                            {
                                                let agents = AGENTS.read().unwrap();
                                                agents.get(&agent_id).map(|v| v.remove_conn(cid, sid));
                                            }
                                            let _ = tokio::io::copy_bidirectional(&mut peer, &mut stream).await;
                                            use tokio::io::AsyncWriteExt;
                                            let _ = peer.shutdown().await;
                                            let _ = stream.shutdown().await;
                                        }
                                        _ => {}
                                    }
                                });
                            }
                        } else {
                            let agents = AGENTS.read().unwrap();
                            agents.get(&agent_id).map(|agent| agent.get_conn_sx(cid, sid).map(|sx| sx.send(stream)));
                        }
                    }
                }
            }
        } => {}
    }
    Ok(())
}

async fn receiving(link: &mut TcpLink) -> std::io::Result<()> {
    let mut pid = link.read.read_compressed_u64().await?;
    if pid <= u32::MAX as _ {
        if (pid as u32) < protocol::BOUDARY {
            let mut client = ClientHandler::new(link.handle().clone());
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
            let mut agent = AgentHandler::new(link.handle().clone());
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
