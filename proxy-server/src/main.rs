#![feature(impl_trait_in_assoc_type)]
#![feature(async_closure)]
#![feature(sync_unsafe_cell)]

use std::{
    collections::HashMap,
    sync::{LazyLock, RwLock},
    time::Duration,
};

use agent::AgentHandler;
use clap::Parser;
use client::ClientHandler;
use conn::Agent;
use file_rotate::{
    compression::Compression,
    suffix::{AppendTimestamp, FileLimit},
    ContentLimit, FileRotate,
};
use slog::{error, info, o, Drain, Logger};
use timeout_stream::TimeoutStream;
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
static LOGGER: LazyLock<Logger> = LazyLock::new(|| {
    let log_path = "/var/log/puty-proxy/proxy-server.log";
    let log = FileRotate::new(
        log_path,
        AppendTimestamp::default(FileLimit::MaxFiles(4)),
        ContentLimit::Lines(30000),
        Compression::None,
        #[cfg(unix)]
        None,
    );
    let decorator = slog_term::PlainDecorator::new(log);
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain)
        .chan_size(4096)
        .overflow_strategy(slog_async::OverflowStrategy::Block)
        .build()
        .fuse();
    slog::Logger::root(drain, o!())
});

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
            let (sender, mut recv) = tokio::sync::mpsc::channel::<TcpStream>(10000);
            let sender_clone = sender.clone();
            tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            let _ = sender_clone.send(stream).await;
                        }
                        Err(e) => {
                            error!(LOGGER, "main listener accept failed: {}", e);
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
                                        Ok(peer) => {
                                            tokio::spawn(async move {
                                                info!(LOGGER, "In the agent.{}, conn.{} begin", agent_id, sid);
                                                // remove conn
                                                {
                                                    let agents = AGENTS.read().unwrap();
                                                    agents.get(&agent_id).map(|v| v.remove_conn(cid, sid));
                                                }

                                                let mut wrap_peer = TimeoutStream::new(peer);
                                                let mut wrap_stream = TimeoutStream::new(stream);
                                                wrap_peer.set_timeout(Some(Duration::from_secs(30)));
                                                wrap_stream.set_timeout(Some(Duration::from_secs(30)));
                                                tokio::pin!(wrap_peer);
                                                tokio::pin!(wrap_stream);
                                                if let Err(_) = tokio::io::copy_bidirectional(&mut wrap_peer, &mut wrap_stream).await {
                                                    use tokio::io::AsyncWriteExt;
                                                    let _ = wrap_peer.shutdown().await;
                                                    let _ = wrap_stream.shutdown().await;
                                                }
                                                info!(LOGGER, "In the agent.{}, conn.{} end", agent_id, sid);
                                            });
                                        }
                                        Err(e) => error!(LOGGER, "rx.await err: {}", e)
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
                error!(LOGGER, "recv invalid pid: {}", pid);
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
                error!(LOGGER, "recv invalid pid: {}", pid);
                return Err(std::io::ErrorKind::InvalidData.into());
            }
        }
    }
    error!(LOGGER, "recv invalid pid: {}", pid);
    Err(std::io::ErrorKind::InvalidData.into())
}
