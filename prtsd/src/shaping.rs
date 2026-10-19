//! Session-local shaping at the encrypted UDP socket boundary. ICE and DTLS (including
//! SCTP control channels) bypass the queue. The driver disables GSO for this wrapper,
//! so every decision, including retransmissions, applies to one actual datagram.
use crate::model::Network;
use std::{
    collections::VecDeque,
    future::Future,
    io::{self, IoSliceMut},
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use webrtc::runtime::{
    AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, EcnCodepoint, JoinHandle,
    RecvMeta, Runtime, TokioRuntime, Transmit,
};
const MAX_QUEUE_BYTES: usize = 4 * 1024 * 1024;
#[derive(Debug)]
struct Datagram {
    socket: Arc<dyn AsyncUdpSocket>,
    data: Vec<u8>,
    destination: SocketAddr,
    ecn: Option<EcnCodepoint>,
    src_ip: Option<IpAddr>,
    due: Instant,
}
struct State {
    settings: Network,
    rng: u64,
    epoch: u64,
    closed: bool,
    queue: VecDeque<Datagram>,
    bytes: usize,
}
pub struct Shaper {
    state: Mutex<State>,
    notify: Notify,
    pub dropped: AtomicU64,
}
impl std::fmt::Debug for Shaper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shaper")
    }
}
impl Shaper {
    pub fn new(settings: &Network) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                settings: settings.clone(),
                rng: settings.seed as u64,
                epoch: 0,
                closed: false,
                queue: VecDeque::new(),
                bytes: 0,
            }),
            notify: Notify::new(),
            dropped: AtomicU64::new(0),
        })
    }
    pub fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move { this.run().await })
    }
    pub fn configure(&self, settings: &Network) {
        let mut s = self.state.lock().unwrap();
        self.dropped
            .fetch_add(s.queue.len() as u64, Ordering::Relaxed);
        s.queue.clear();
        s.bytes = 0;
        s.epoch += 1;
        s.rng = settings.seed as u64;
        s.settings = settings.clone();
        drop(s);
        self.notify.notify_one();
    }
    pub fn close(&self) {
        let mut s = self.state.lock().unwrap();
        s.closed = true;
        s.queue.clear();
        s.bytes = 0;
        s.epoch += 1;
        drop(s);
        self.notify.notify_one();
    }
    async fn run(self: Arc<Self>) {
        loop {
            // Register before observing state to avoid a settings/close notification race.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let due = {
                let s = self.state.lock().unwrap();
                if s.closed {
                    return;
                }
                s.queue.front().map(|p| p.due)
            };
            let Some(due) = due else {
                notified.await;
                continue;
            };
            tokio::select! { _=&mut notified=>continue, _=tokio::time::sleep_until(due.into())=>{} }
            let (packet, epoch) = {
                let mut s = self.state.lock().unwrap();
                if s.closed {
                    return;
                }
                let Some(p) = s.queue.pop_front() else {
                    continue;
                };
                (p, s.epoch)
            };
            let send = std::future::poll_fn(|cx| {
                let s = self.state.lock().unwrap();
                if s.closed || s.epoch != epoch {
                    return Poll::Ready(Ok(0));
                }
                packet.socket.poll_send(
                    cx,
                    &Transmit {
                        destination: packet.destination,
                        ecn: packet.ecn,
                        contents: &packet.data,
                        segment_size: None,
                        src_ip: packet.src_ip,
                    },
                )
            });
            let result = tokio::time::timeout(Duration::from_millis(100), send).await;
            if !matches!(result, Ok(Ok(_))) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            let mut s = self.state.lock().unwrap();
            if s.epoch == epoch {
                s.bytes -= packet.data.len();
            }
        }
    }
}
fn is_media(data: &[u8]) -> bool {
    data.len() >= 12 && (128..=191).contains(&data[0])
}
#[derive(Debug)]
struct Socket {
    inner: Arc<dyn AsyncUdpSocket>,
    shaper: Arc<Shaper>,
}
impl AsyncUdpSocket for Socket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }
    fn max_gro_segments(&self) -> usize {
        self.inner.max_gro_segments()
    }
    fn poll_send(&self, cx: &mut Context<'_>, p: &Transmit<'_>) -> Poll<io::Result<usize>> {
        if !is_media(p.contents) {
            return self.inner.poll_send(cx, p);
        }
        let mut s = self.shaper.state.lock().unwrap();
        if s.closed {
            return Poll::Ready(Ok(p.contents.len()));
        }
        // SplitMix64: deterministic per session and seed, including seed zero.
        s.rng = s.rng.wrapping_add(0x9e3779b97f4a7c15);
        let mut x = s.rng;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        x ^= x >> 31;
        let lose = (x >> 11) as f64 / ((1u64 << 53) as f64) * 100.0 < s.settings.loss_percent;
        if lose {
            self.shaper.dropped.fetch_add(1, Ordering::Relaxed);
            return Poll::Ready(Ok(p.contents.len()));
        }
        while s.bytes + p.contents.len() > MAX_QUEUE_BYTES {
            let Some(oldest) = s.queue.pop_front() else {
                self.shaper.dropped.fetch_add(1, Ordering::Relaxed);
                return Poll::Ready(Ok(p.contents.len()));
            };
            s.bytes -= oldest.data.len();
            self.shaper.dropped.fetch_add(1, Ordering::Relaxed);
        }

        let due = Instant::now() + Duration::from_millis(s.settings.added_latency_ms as u64);
        s.bytes += p.contents.len();
        s.queue.push_back(Datagram {
            socket: self.inner.clone(),
            data: p.contents.to_vec(),
            destination: p.destination,
            ecn: p.ecn,
            src_ip: p.src_ip,
            due,
        });
        drop(s);
        self.shaper.notify.notify_one();
        Poll::Ready(Ok(p.contents.len()))
    }
}
#[derive(Debug)]
pub struct ShapedRuntime(pub Arc<Shaper>);
impl Runtime for ShapedRuntime {
    fn spawn(&self, f: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn(f)
    }
    fn spawn_reactor(
        &self,
        n: usize,
        f: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn_reactor(n, f)
    }
    fn wrap_udp_socket(&self, s: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Ok(Arc::new(Socket {
            inner: TokioRuntime.wrap_udp_socket(s)?,
            shaper: self.0.clone(),
        }))
    }
    fn wrap_tcp_listener(&self, s: std::net::TcpListener) -> io::Result<Arc<dyn AsyncTcpListener>> {
        TokioRuntime.wrap_tcp_listener(s)
    }
    fn connect_tcp<'a>(
        &'a self,
        a: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        TokioRuntime.connect_tcp(a)
    }
    fn resolve_host<'a>(
        &'a self,
        h: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        TokioRuntime.resolve_host(h)
    }
    fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        TokioRuntime.sleep(d)
    }
    fn interval(&self, d: Duration) -> Box<dyn AsyncInterval> {
        TokioRuntime.interval(d)
    }
    fn block_on(&self, f: Pin<Box<dyn Future<Output = ()> + '_>>) {
        TokioRuntime.block_on(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings(latency: u32, loss: f64, seed: u32) -> Network {
        Network {
            bitrate_cap_bps: None,
            added_latency_ms: latency,
            loss_percent: loss,
            seed,
        }
    }
    async fn socket(shaper: Arc<Shaper>) -> Socket {
        let raw = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        raw.set_nonblocking(true).unwrap();
        Socket {
            inner: TokioRuntime.wrap_udp_socket(raw).unwrap(),
            shaper,
        }
    }
    #[tokio::test]
    async fn delayed_media_control_bypass_and_flush() {
        let shaper = Shaper::new(&settings(90, 0.0, 0));
        let task = shaper.start();
        let socket = socket(shaper.clone()).await;
        let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = receiver.local_addr().unwrap();
        let media = [128u8; 100];
        let mut buf = [0; 200];
        let start = Instant::now();
        socket.send_to(&media, addr).await.unwrap();
        socket.send_to(&[22u8; 13], addr).await.unwrap();
        receiver.recv(&mut buf).await.unwrap();
        assert_eq!(buf[0], 22);
        assert!(
            tokio::time::timeout(Duration::from_millis(40), receiver.recv(&mut buf))
                .await
                .is_err()
        );
        receiver.recv(&mut buf).await.unwrap();
        assert_eq!(buf[0], 128);
        assert!(start.elapsed() >= Duration::from_millis(80));
        socket.send_to(&media, addr).await.unwrap();
        shaper.configure(&settings(0, 100.0, 7));
        assert!(
            tokio::time::timeout(Duration::from_millis(120), receiver.recv(&mut buf))
                .await
                .is_err()
        );
        socket.send_to(&media, addr).await.unwrap();
        socket.send_to(&[0u8; 20], addr).await.unwrap();
        receiver.recv(&mut buf).await.unwrap();
        assert_eq!(buf[0], 0);
        assert!(shaper.dropped.load(Ordering::Relaxed) >= 2);
        shaper.close();
        task.await.unwrap();
    }
    #[tokio::test]
    async fn queue_bound_and_seed_replay() {
        let shaper = Shaper::new(&settings(2000, 35.0, 42));
        let socket = socket(shaper.clone()).await;
        let addr = "127.0.0.1:9".parse().unwrap();
        let media = vec![128; 1400];
        let mut first = Vec::new();
        for _ in 0..100 {
            socket.send_to(&media, addr).await.unwrap();
            first.push(shaper.dropped.load(Ordering::Relaxed));
        }
        shaper.configure(&settings(2000, 35.0, 42));
        shaper.dropped.store(0, Ordering::Relaxed);
        let mut second = Vec::new();
        for _ in 0..100 {
            socket.send_to(&media, addr).await.unwrap();
            second.push(shaper.dropped.load(Ordering::Relaxed));
        }
        assert_eq!(first, second);
        shaper.configure(&settings(2000, 0.0, 0));
        let mut media = media;
        for index in 0..4000u32 {
            media[1..5].copy_from_slice(&index.to_le_bytes());
            socket.send_to(&media, addr).await.unwrap();
        }
        let state = shaper.state.lock().unwrap();
        assert!(state.bytes <= MAX_QUEUE_BYTES);
        let oldest = &state.queue.front().unwrap().data;
        assert_eq!(
            u32::from_le_bytes(oldest[1..5].try_into().unwrap()),
            4000 - (MAX_QUEUE_BYTES / 1400) as u32
        );
        assert!(shaper.dropped.load(Ordering::Relaxed) > 0);
        drop(state);
        shaper.close();
        assert_eq!(shaper.state.lock().unwrap().bytes, 0);
    }
}
