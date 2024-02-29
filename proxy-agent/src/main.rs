#![feature(async_closure)]
#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]

use std::{
    future::Future,
    net::Ipv4Addr,
    process::exit,
    sync::{atomic::AtomicU32, Arc, LazyLock},
    time::Duration,
};

use clap::Parser;
use dashmap::DashMap;
use file_rotate::{compression::Compression, suffix::{AppendTimestamp, FileLimit}, ContentLimit, FileRotate};
use protocol::{
    compose, PacketHbAgent, PacketInfoClientClosed, PacketInfoConnectFailed, ReqAgentBuild,
    ReqAgentLogin, ReqNewConnectionAgent, RspAgentBuild, RspAgentLoginFailed, RspAgentLoginOk,
    RspClientNotFound,
};
use slog::{error, info, o, Drain, Logger};
use timeout_stream::TimeoutStream;
use tokio::{
    io::BufReader,
    net::{tcp::OwnedReadHalf, TcpListener, TcpStream},
    task::JoinHandle,
};
use vnpkt::tokio_ext::{
    io::{AsyncReadExt, AsyncWriteExt},
    registry::{PacketProc, Registry, RegistryInit},
};
use vnsvrbase::{
    process::hook_terminate_signal,
    tokio_ext::tcp_link::{send_pkt, TcpLink},
};

static REGISTRY: LazyLock<Registry<Handler>> = LazyLock::new(Registry::new);
static PROXYS: LazyLock<DashMap<u32, (u16, JoinHandle<()>, Vec<(u32, JoinHandle<()>)>)>> =
    LazyLock::new(DashMap::new);
static LOGGER: LazyLock<Logger> = LazyLock::new(|| {
    let log_path = "/var/log/puty-proxy/proxy-agent.log";
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

type Conns = DashMap<u32, DashMap<u32, TcpStream>>;

#[derive(Parser)]
struct Args {
    server: String,
    id: u32,
    server_main_port: u16,
    server_conn_port: u16,
}

fn main() {
    let args = Args::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_io()
        .enable_time()
        .build()
        .unwrap();
    let (quit_tx, mut quit_rx) = tokio::sync::watch::channel(false);
    hook_terminate_signal(Some(move || {
        let _ = quit_tx.send(true);
    }));

    rt.block_on(async {
        if let Err(_) = main_loop(args).await {
            error!(LOGGER, "connect server failed");
            exit(-1);
        }
        let _ = quit_rx.changed().await;
    });
}

async fn main_loop(args: Args) -> std::io::Result<()> {
    let stream = TcpStream::connect((args.server.as_str(), args.server_main_port)).await?;
    info!(LOGGER, "connect server success");
    let handle = tokio::runtime::Handle::current();
    let _ = TcpLink::attach(stream, &handle, &handle, async move |link: &mut TcpLink| {
        let _ = send_pkt!(link.handle(), ReqAgentLogin { id: args.id });
        receiving(link, args).await
    });
    Ok(())
}

async fn receiving(link: &mut TcpLink, args: Args) -> std::io::Result<()> {
    let mut handler = Handler {
        handle: link.handle().clone(),
        conns: Arc::new(DashMap::new()),
        seed: Arc::new(AtomicU32::new(0)),
        args,
    };
    let cnt = AtomicU32::new(0);

    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(20)) => {
                if cnt.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 3 {
                    info!(LOGGER, "recv from server timeout 3 times, conn closed");
                    link.handle().close();
                    exit(-1);
                }
            }
            res = async {
                let pid = link.read.read_compressed_u64().await?;
                cnt.store(0, std::sync::atomic::Ordering::SeqCst);
                let register = &*REGISTRY;

                if pid > u32::MAX as u64 {
                    error!(LOGGER, "recv wrong pid = {}", pid);
                    std::io::Result::<()>::Err(std::io::ErrorKind::InvalidData.into())
                } else if let Some(item) = register.query(pid as u32) {
                    let r = item.recv(&mut link.read).await?;
                    r.proc(&mut handler).await?;
                    Ok(())
                } else {
                    error!(LOGGER, "recv wrong pid = {}", pid);
                    std::io::Result::<()>::Err(std::io::ErrorKind::InvalidData.into())
                }
            } => {
                if let Err(e) = res {
                    link.handle().close();
                    error!(LOGGER, "{}", e);
                    exit(-1);
                }
            }
        }
    }
}

struct Handler {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    conns: Arc<Conns>,
    seed: Arc<AtomicU32>,
    args: Args,
}

impl RegistryInit for Handler {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<PacketHbAgent>();
        register.insert::<RspAgentLoginOk>();
        register.insert::<RspAgentLoginFailed>();
        register.insert::<ReqAgentBuild>();
        register.insert::<RspClientNotFound>();
        register.insert::<ReqNewConnectionAgent>();
        register.insert::<PacketInfoClientClosed>();
    }
}

impl PacketProc<PacketHbAgent> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<PacketHbAgent>) -> Self::Output<'_> {
        async { Ok(()) }
    }
}

impl PacketProc<RspAgentLoginOk> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspAgentLoginOk>) -> Self::Output<'_> {
        async {
            let handle = self.handle.clone();
            let id = self.args.id;
            tokio::spawn(async move {
                loop {
                    let _ = send_pkt!(handle, PacketHbAgent { id });
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });
            Ok(())
        }
    }
}

impl PacketProc<RspAgentLoginFailed> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspAgentLoginFailed>) -> Self::Output<'_> {
        async {
            info!(LOGGER, "login server failed");
            self.handle.close();
            exit(-1);
        }
    }
}

impl PacketProc<ReqAgentBuild> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqAgentBuild>) -> Self::Output<'_> {
        async {
            let handle = self.handle.clone();
            let conns = self.conns.clone();
            let seed = self.seed.clone();
            let port = pkt.port;
            let cid = pkt.id;
            let agent_id = self.args.id;

            let proxy = tokio::spawn(async move {
                match TcpListener::bind((Ipv4Addr::UNSPECIFIED, pkt.port)).await {
                    Ok(listener) => {
                        info!(LOGGER, "agent-{} on port {} begin", agent_id, pkt.port);
                        loop {
                            if let Ok((stream, _)) = listener.accept().await {
                                let _ = stream.set_nodelay(true);
                                let _ = stream.set_linger(None);
                                let sid = seed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                                use dashmap::mapref::entry::Entry;
                                match conns.entry(pkt.id) {
                                    Entry::Vacant(e) => {
                                        let conns = DashMap::new();
                                        conns.insert(sid, stream);
                                        e.insert(conns);
                                    }
                                    Entry::Occupied(e) => {
                                        e.get().insert(sid, stream);
                                    }
                                }
                                info!(LOGGER, "In agent-{} of port {}, local conn.{} build", agent_id, pkt.port, sid);
                                // send to server
                                let _ = send_pkt!(
                                    handle,
                                    RspAgentBuild {
                                        agent_id,
                                        id: pkt.id,
                                        sid,
                                        ok: true
                                    }
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let _ = send_pkt!(
                            handle,
                            RspAgentBuild {
                                agent_id,
                                id: pkt.id,
                                sid: 0,
                                ok: false
                            }
                        );
                        error!(LOGGER, "agent-build failed: {}", e);
                    }
                }
            });

            PROXYS.insert(cid, (port, proxy, Vec::new()));
            Ok(())
        }
    }
}

impl PacketProc<ReqNewConnectionAgent> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqNewConnectionAgent>) -> Self::Output<'_> {
        let agent_id = self.args.id;
        let cid = pkt.id;
        let sid = pkt.sid;
        async move {
            if let Ok(mut remote) =
                TcpStream::connect((self.args.server.as_str(), self.args.server_conn_port)).await
            {
                if async {
                    use tokio::io::AsyncWriteExt;
                    remote.write_u32(self.args.id).await?;
                    remote.write_u32(pkt.id).await?;
                    remote.write_compressed_u64(compose(pkt.sid, false)).await?;
                    std::io::Result::Ok(())
                }
                .await
                .is_ok()
                {
                    if let Some(conns) = self.conns.get(&pkt.id) {
                        match conns.remove(&pkt.sid) {
                            Some((_, local)) => {
                                let task = tokio::spawn(async move {
                                    let mut wrap_local = TimeoutStream::new(local);
                                    let mut wrap_remote = TimeoutStream::new(remote);
                                    wrap_local.set_timeout(Some(Duration::from_secs(30)));
                                    wrap_remote.set_timeout(Some(Duration::from_secs(30)));
                                    tokio::pin!(wrap_local);
                                    tokio::pin!(wrap_remote);
                                    info!(LOGGER, "In agent-{}, conn.{} begin with cid = {}", agent_id, sid, cid);
                                    if let Err(_) = tokio::io::copy_bidirectional(
                                        &mut wrap_local,
                                        &mut wrap_remote,
                                    )
                                    .await
                                    {
                                        use tokio::io::AsyncWriteExt;
                                        let _ = wrap_local.shutdown().await;
                                        let _ = wrap_remote.shutdown().await;
                                    }
                                    info!(LOGGER, "In agent-{}, conn.{} end with cid = {}", agent_id, sid, cid);
                                });

                                use dashmap::mapref::entry::Entry;
                                match PROXYS.entry(pkt.id) {
                                    Entry::Occupied(mut e) => {
                                        e.get_mut().2.push((pkt.sid, task));
                                    }
                                    _ => error!(LOGGER, "cid = {} already exist", pkt.id)
                                }
                            }
                            _ => error!(LOGGER, "can't find conns with sid = {}", pkt.sid)
                        }
                    }
                }
            } else {
                use tokio::io::AsyncWriteExt;
                if let Some(conns) = self.conns.get(&cid) {
                    match conns.remove(&sid) {
                        Some((_, mut local)) => {
                            let _ = local.shutdown().await;
                        }
                        _ => {}
                    }
                }
                let _ = send_pkt!(self.handle, PacketInfoConnectFailed { agent_id, cid, sid });
                error!(LOGGER, "connect server conn port failed");
            }
            Ok(())
        }
    }
}

impl PacketProc<RspClientNotFound> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspClientNotFound>) -> Self::Output<'_> {
        async move { self.clear(pkt.id).await }
    }
}

impl PacketProc<PacketInfoClientClosed> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketInfoClientClosed>) -> Self::Output<'_> {
        async move { self.clear(pkt.id).await }
    }
}

impl Handler {
    async fn clear(&mut self, id: u32) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        self.conns.remove(&id).map(async move |v| {
            for mut v in v.1 {
                let _ = v.1.shutdown().await;
            }
        });
        if let Some((_, (port, listen, conns))) = PROXYS.remove(&id) {
            listen.abort();
            for (_, v) in conns {
                v.abort();
            }
            info!(LOGGER, "agent-{} on port {} shutdown", self.args.id, port);
        }
        Ok(())
    }
}
