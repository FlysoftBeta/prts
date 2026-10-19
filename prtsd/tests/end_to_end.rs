//! Real child-process/UDP/DTLS/SRTP/SCTP test; independent of server/ and Chromium.
use anyhow::{Context, Result};
use openh264::{decoder::Decoder, formats::YUVSource};
use rtc::{
    rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication,
    rtp::{codec::h264::H264Packet, packetizer::Depacketizer},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{Mutex as AsyncMutex, mpsc, oneshot},
};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    media_stream::track_remote::{TrackRemote, TrackRemoteEvent},
    peer_connection::{
        MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
        RTCIceCandidateInit, RTCIceGatheringState, RTCPeerConnectionIceEvent,
        RTCSessionDescription, Registry, register_default_interceptors,
    },
};

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;
#[derive(Default)]
struct Bridge {
    pc: Option<Arc<dyn PeerConnection>>,
    pending: Vec<RTCIceCandidateInit>,
}
#[derive(Clone)]
struct Control {
    input: Arc<AsyncMutex<Option<ChildStdin>>>,
    pending: Pending,
    next: Arc<AtomicU64>,
    log: Arc<Mutex<Vec<Value>>>,
    bridge: Arc<AsyncMutex<Bridge>>,
}
impl Control {
    async fn start() -> Result<(Self, Child)> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_prtsd"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdout = child.stdout.take().unwrap();
        let c = Self {
            input: Arc::new(AsyncMutex::new(child.stdin.take())),
            pending: Default::default(),
            next: Arc::new(AtomicU64::new(0)),
            log: Default::default(),
            bridge: Default::default(),
        };
        let reader = c.clone();
        tokio::spawn(async move {
            while let Ok(len) = stdout.read_u32_le().await {
                assert!((1..=4194304).contains(&len), "unframed stdout");
                let mut data = vec![0; len as usize];
                stdout.read_exact(&mut data).await.unwrap();
                let value: Value = serde_json::from_slice(&data).expect("stdout must be JSON");
                reader.log.lock().unwrap().push(value.clone());
                if let Some(id) = value["id"].as_str() {
                    if let Some(tx) = reader.pending.lock().unwrap().remove(id) {
                        let _ = tx.send(value.clone());
                    }
                } else if value["event"] == "session.ice" {
                    let candidate = if value["data"]["candidate"].is_null() {
                        RTCIceCandidateInit::default()
                    } else {
                        serde_json::from_value(value["data"]["candidate"].clone()).unwrap()
                    };
                    let mut b = reader.bridge.lock().await;
                    if let Some(pc) = &b.pc {
                        pc.add_ice_candidate(candidate).await.unwrap();
                    } else {
                        b.pending.push(candidate);
                    }
                }
            }
        });
        Ok((c, child))
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_fragmented(method, params, false).await
    }
    async fn request_fragmented(
        &self,
        method: &str,
        params: Value,
        fragment: bool,
    ) -> Result<Value> {
        let id = format!("r{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let data = serde_json::to_vec(&json!({"v":1,"id":id,"method":method,"params":params}))?;
        let packet = [
            (data.len() as u32).to_le_bytes().as_slice(),
            data.as_slice(),
        ]
        .concat();
        {
            let mut input = self.input.lock().await;
            let input = input.as_mut().unwrap();
            if fragment {
                for b in packet {
                    input.write_all(&[b]).await?;
                    tokio::task::yield_now().await;
                }
            } else {
                input.write_all(&packet).await?;
            }
            input.flush().await?;
        }
        Ok(tokio::time::timeout(Duration::from_secs(8), rx)
            .await
            .context("response timeout")??)
    }
    async fn ok(&self, method: &str, params: Value) -> Result<Value> {
        let v = self.request(method, params).await?;
        assert!(v.get("error").is_none(), "{method}: {v}");
        Ok(v["result"].clone())
    }
    async fn wait_event(&self, name: &str) -> Result<Value> {
        wait_for(|| {
            self.log
                .lock()
                .unwrap()
                .iter()
                .find(|v| v["event"] == name)
                .cloned()
        })
        .await
    }
}
async fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> Result<T> {
    let end = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(v) = f() {
            return Ok(v);
        }
        anyhow::ensure!(Instant::now() < end, "condition timed out");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}
#[derive(Clone, Debug)]
struct Picture {
    w: usize,
    h: usize,
    dot: u8,
    text_pixels: usize,
    at: Instant,
}
#[derive(Clone)]
struct ClientHandler {
    ice: mpsc::Sender<Option<RTCIceCandidateInit>>,
    messages: Arc<Mutex<Vec<Value>>>,
    control: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    pictures: Arc<Mutex<Vec<Picture>>>,
    rtp: Arc<AtomicU64>,
    plis: Arc<AtomicU64>,
    idrs: Arc<AtomicU64>,
    feedback: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for ClientHandler {
    async fn on_ice_candidate(&self, e: RTCPeerConnectionIceEvent) {
        self.ice
            .send(Some(e.candidate.to_json().unwrap()))
            .await
            .unwrap();
    }
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.ice.send(None).await;
        }
    }
    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        if dc.label().await.unwrap() == "prts.control.v1" {
            *self.control.lock().unwrap() = Some(dc.clone());
        } else if dc.label().await.unwrap() == "prts.feedback.v1" {
            *self.feedback.lock().unwrap() = Some(dc.clone());
        }
        let messages = self.messages.clone();
        tokio::spawn(async move {
            while let Some(event) = dc.poll().await {
                if let DataChannelEvent::OnMessage(m) = event {
                    let value: Value = serde_json::from_slice(&m.data).unwrap();
                    messages.lock().unwrap().push(value);
                }
            }
        });
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let (pictures, rtp, plis, idrs) = (
            self.pictures.clone(),
            self.rtp.clone(),
            self.plis.clone(),
            self.idrs.clone(),
        );
        tokio::spawn(async move {
            let mut depacketizer = H264Packet::default();
            let mut decoder = Decoder::new().unwrap();
            let mut encoded = Vec::new();
            let mut requested = false;
            while let Some(event) = track.poll().await {
                if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                    rtp.fetch_add(1, Ordering::Relaxed);
                    if let Ok(data) = depacketizer.depacketize(&packet.payload) {
                        encoded.extend_from_slice(&data);
                    }
                    if packet.header.marker {
                        if encoded
                            .windows(4)
                            .any(|w| w[..3] == [0, 0, 1] && w[3] & 31 == 5)
                        {
                            idrs.fetch_add(1, Ordering::Relaxed);
                        }
                        if let Ok(Some(yuv)) = decoder.decode(&encoded) {
                            let (w, h) = yuv.dimensions();
                            let stride = yuv.strides().0;
                            let y = yuv.y();
                            let dot = if w > 250 && h > 180 {
                                y[180 * stride + 250]
                            } else {
                                0
                            };
                            let text_pixels = (36..44.min(h))
                                .flat_map(|row| (48..300.min(w)).map(move |x| (row, x)))
                                .filter(|(row, x)| y[row * stride + x] > 180)
                                .count();
                            pictures.lock().unwrap().push(Picture {
                                w,
                                h,
                                dot,
                                text_pixels,
                                at: Instant::now(),
                            });
                        }
                        encoded.clear();
                        if !requested {
                            requested = true;
                            plis.fetch_add(1, Ordering::Relaxed);
                            track
                                .write_rtcp(vec![Box::new(PictureLossIndication {
                                    sender_ssrc: 42,
                                    media_ssrc: packet.header.ssrc,
                                })])
                                .await
                                .unwrap();
                        }
                    }
                }
            }
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synthetic_stdio_webrtc_input_and_lifecycle() -> Result<()> {
    let (c, mut child) = Control::start().await?;
    let hello = c
        .request_fragmented("hello", json!({"versions":[1]}), true)
        .await?;
    assert_eq!(hello["result"]["version"], 1);
    assert_eq!(hello["result"]["capabilities"]["packet_shaping"], true);
    c.ok("stats.configure", json!({"interval_ms":250})).await?;
    assert_eq!(
        c.request("instance.create", json!({"options":{"source":"chromium"}}))
            .await?["error"]["code"],
        "unsupported"
    );
    let instance = c
        .ok(
            "instance.create",
            json!({"options":{"encoder":"software","size":{"width":320,"height":240},"fps":30}}),
        )
        .await?;
    assert_eq!(instance["instance"]["state"], "starting");
    let iid = instance["instance"]["instance_id"].as_str().unwrap();
    c.wait_event("instance.ready").await?;
    {
        let log = c.log.lock().unwrap();
        let response = log
            .iter()
            .position(|v| v["result"].get("instance").is_some())
            .unwrap();
        let created = log
            .iter()
            .position(|v| v["event"] == "target.created")
            .unwrap();
        let ready = log
            .iter()
            .position(|v| v["event"] == "instance.ready")
            .unwrap();
        assert!(response < created && created < ready);
        assert_eq!(log[created]["data"]["target"]["load"], "loading");
    }
    let session = c
        .ok(
            "session.create",
            json!({"instance_id":iid,"viewer_id":"self-test","control":true}),
        )
        .await?;
    let sid = session["session_id"].as_str().unwrap().to_owned();
    assert_eq!(
        c.request(
            "session.create",
            json!({"instance_id":iid,"viewer_id":"second-owner","control":true})
        )
        .await?["error"]["code"],
        "permission_denied"
    );
    let (ice_tx, mut ice_rx) = mpsc::channel(256);
    let handler = ClientHandler {
        ice: ice_tx,
        messages: Default::default(),
        control: Default::default(),
        pictures: Default::default(),
        rtp: Default::default(),
        plis: Default::default(),
        idrs: Default::default(),
        feedback: Default::default(),
    };
    let mut media = MediaEngine::default();
    media.register_default_codecs()?;
    let registry = register_default_interceptors(Registry::new(), &mut media)?;
    let pc: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_handler(Arc::new(handler.clone()))
            .with_udp_addrs(vec!["0.0.0.0:0"])
            .build()
            .await?,
    );
    let offer = c.ok("session.offer", json!({"session_id":sid})).await?;
    assert_eq!(offer["description"]["type"], "offer");
    assert!(
        offer["description"]["sdp"]
            .as_str()
            .unwrap()
            .contains("H264/90000")
    );
    assert_eq!(
        c.request("session.offer", json!({"session_id":sid}))
            .await?["error"]["code"],
        "invalid_state"
    );
    pc.set_remote_description(serde_json::from_value::<RTCSessionDescription>(
        offer["description"].clone(),
    )?)
    .await?;
    {
        let mut b = c.bridge.lock().await;
        b.pc = Some(pc.clone());
        for candidate in std::mem::take(&mut b.pending) {
            pc.add_ice_candidate(candidate).await?;
        }
    }
    let answer = pc.create_answer(None).await?;
    assert_eq!(serde_json::to_value(&answer)?["type"], "answer");
    let ufrag = prtsd::viewer::sdp_field(&answer.sdp, "a=ice-ufrag:").unwrap();
    let local_ufrag = Arc::new(Mutex::new(ufrag));
    let ice_ufrag = local_ufrag.clone();
    let ice_control = c.clone();
    let ice_sid = sid.clone();
    let ice_task = tokio::spawn(async move {
        while let Some(candidate) = ice_rx.recv().await {
            let candidate = candidate.map(|mut c| {
                c.username_fragment = Some(ice_ufrag.lock().unwrap().clone());
                c.sdp_mid = Some("0".into());
                c.sdp_mline_index = Some(0);
                c
            });
            ice_control
                .ok(
                    "session.ice",
                    json!({"session_id":ice_sid,"candidate":candidate}),
                )
                .await
                .unwrap();
        }
    });
    pc.set_local_description(answer.clone()).await?;
    c.ok(
        "session.answer",
        json!({"session_id":sid,"description":answer}),
    )
    .await?;
    wait_for(|| {
        handler
            .messages
            .lock()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "ready")
            .cloned()
    })
    .await?;
    // A PLI sent after the first frame must produce another IDR well before the 2s periodic IDR.
    tokio::time::timeout(
        Duration::from_millis(1500),
        wait_for(|| (handler.idrs.load(Ordering::Acquire) >= 2).then_some(())),
    )
    .await??;
    let before = wait_for(|| handler.pictures.lock().unwrap().last().cloned()).await?;
    assert_eq!((before.w, before.h), (320, 240));
    let dc = handler.control.lock().unwrap().clone().unwrap();
    dc.send_text(&json!({"v":1,"type":"mouse.move","target_id":"1","generation":"1","sequence":1,"modifiers":0,"x":250,"y":180,"buttons":0}).to_string()).await?;
    dc.send_text(&json!({"v":1,"type":"key.char","target_id":"1","generation":"1","sequence":2,"modifiers":0,"text":"Hi\u{4e2d}"}).to_string()).await?;
    let input_at = Instant::now();
    let changed = wait_for(|| {
        handler
            .pictures
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|p| p.at > input_at && p.dot > 190 && p.text_pixels > before.text_pixels + 15)
            .cloned()
    })
    .await?;
    let feedback = wait_for(|| handler.feedback.lock().unwrap().clone()).await?;
    assert!(!feedback.ordered().await?);
    // rtc 0.21.1 does not populate remote max_retransmits metadata from DCEP;
    // the sender's Some(0) still selects unordered partial reliability on the wire.
    assert_eq!(feedback.protocol().await?, "prts-json-v1");
    feedback.send_text(&json!({"v":1,"type":"stats","sample_seq":1,"interval_ms":250,"decoded_fps":30,"received_bps":100000,"rtt_ms":1,"jitter_ms":0,"packets_lost":0,"decode_ms_avg":1,"render_ms_avg":null}).to_string()).await?;
    dc.send_text(&json!({"v":1,"type":"mouse.move","target_id":"1","generation":"999","sequence":4,"modifiers":0,"x":50,"y":50,"buttons":0}).to_string()).await?;
    wait_for(|| {
        handler
            .messages
            .lock()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "error" && v["error"]["code"] == "stale_state")
            .cloned()
    })
    .await?;
    assert!(
        changed.dot > before.dot + 50,
        "pointer content must change decoded pixels"
    );
    dc.send_text(&json!({"v":1,"type":"mouse.move","target_id":"1","generation":"1","sequence":5,"modifiers":0,"x":140,"y":120,"buttons":0}).to_string()).await?;
    wait_for(|| {
        handler
            .messages
            .lock()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "cursor" && v["cursor"]["shape"] == "pointer")
            .cloned()
    })
    .await?;
    let stats = wait_for(|| {
        c.log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|v| {
                v["event"] == "stats"
                    && v["data"]["frames"]["encoded"].as_u64().unwrap_or(0) > 0
                    && v["data"]["sessions"][0]["rtp_sent_bps"]
                        .as_f64()
                        .unwrap_or(0.0)
                        > 0.0
            })
            .cloned()
    })
    .await?;
    assert!(stats["data"]["encoder"]["fps"].as_f64().unwrap() > 0.0);
    assert!(stats["data"]["frames"]["capture_to_encode_ms_avg"].is_number());
    wait_for(|| {
        c.log
            .lock()
            .unwrap()
            .iter()
            .find(|v| {
                v["event"] == "stats"
                    && v["data"]["sessions"][0]["viewer_feedback"]["sample_seq"] == 1
            })
            .cloned()
    })
    .await?;
    let initial_ufrag = prtsd::viewer::sdp_field(
        offer["description"]["sdp"].as_str().unwrap(),
        "a=ice-ufrag:",
    );
    for restart in [false, true] {
        c.bridge.lock().await.pc = None;
        let offer = c
            .ok(
                "session.offer",
                json!({"session_id":sid,"ice_restart":restart}),
            )
            .await?;
        let desc: RTCSessionDescription = serde_json::from_value(offer["description"].clone())?;
        if restart {
            assert_ne!(
                prtsd::viewer::sdp_field(&desc.sdp, "a=ice-ufrag:"),
                initial_ufrag
            );
        }
        pc.set_remote_description(desc).await?;
        {
            let mut b = c.bridge.lock().await;
            b.pc = Some(pc.clone());
            for candidate in std::mem::take(&mut b.pending) {
                pc.add_ice_candidate(candidate).await?;
            }
        }
        let answer = pc.create_answer(None).await?;
        *local_ufrag.lock().unwrap() =
            prtsd::viewer::sdp_field(&answer.sdp, "a=ice-ufrag:").unwrap();
        pc.set_local_description(answer.clone()).await?;
        c.ok(
            "session.answer",
            json!({"session_id":sid,"description":answer}),
        )
        .await?;
        let packets = handler.rtp.load(Ordering::Acquire);
        wait_for(|| (handler.rtp.load(Ordering::Acquire) > packets + 5).then_some(())).await?;
    }
    c.ok("network.set",json!({"instance_id":iid,"settings":{"bitrate_cap_bps":800000,"added_latency_ms":0,"loss_percent":0,"seed":42}})).await?;
    c.ok("network.set",json!({"instance_id":iid,"settings":{"bitrate_cap_bps":800000,"added_latency_ms":80,"loss_percent":2.5,"seed":42}})).await?;
    let second = c
        .ok(
            "target.create",
            json!({"instance_id":iid,"url":"https://example.org/","activate":false}),
        )
        .await?;
    assert_eq!(second["target"]["target_id"], "2");
    assert_eq!(second["target"]["active"], false);
    c.ok(
        "target.navigate",
        json!({"instance_id":iid,"target_id":"2","url":"https://example.net/"}),
    )
    .await?;
    let active = c
        .ok(
            "target.activate",
            json!({"instance_id":iid,"target_id":"2"}),
        )
        .await?;
    assert_eq!(active["target"]["generation"], "2");
    let resized = c
        .ok(
            "target.resize",
            json!({"instance_id":iid,"target_id":"2","size":{"width":640,"height":360}}),
        )
        .await?;
    assert_eq!(resized["target"]["generation"], "3");
    wait_for(|| {
        handler
            .pictures
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.w == 640 && p.h == 360)
            .cloned()
    })
    .await?;
    let list = c.ok("target.list", json!({"instance_id":iid})).await?;
    assert_eq!(list["targets"].as_array().unwrap().len(), 2);
    c.ok("target.close", json!({"instance_id":iid,"target_id":"2"}))
        .await?;
    c.ok("target.close", json!({"instance_id":iid,"target_id":"1"}))
        .await?;
    wait_for(|| {
        handler
            .messages
            .lock()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "target" && v["target"].is_null())
            .cloned()
    })
    .await?;
    let third = c.ok("target.create", json!({"instance_id":iid})).await?;
    assert_eq!(third["target"]["target_id"], "3");
    c.ok("session.close", json!({"session_id":sid})).await?;
    pc.close().await?;
    ice_task.abort();
    c.ok("instance.close", json!({"instance_id":iid})).await?;
    assert_eq!(
        c.ok("instance.list", json!({})).await?["instances"],
        json!([])
    );
    c.input.lock().await.take();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await??
            .success()
    );
    assert!(handler.rtp.load(Ordering::Relaxed) > 10);
    assert!(handler.plis.load(Ordering::Relaxed) > 0);
    let log = c.log.lock().unwrap();
    let seq: Vec<_> = log.iter().filter_map(|v| v["seq"].as_u64()).collect();
    assert!(seq.windows(2).all(|s| s[0] < s[1]));
    eprintln!(
        "self-loop: {} RTP packets, {} decoded frames; pointer/text pixels, resize, trickle ICE, stats and shutdown passed",
        handler.rtp.load(Ordering::Relaxed),
        handler.pictures.lock().unwrap().len()
    );
    Ok(())
}

#[test]
fn framing_limits_and_coalescing() {
    use prtsd::transport::{read_record, write_record};
    use std::io::{Cursor, Read};
    struct Fragmented(Cursor<Vec<u8>>);
    impl Read for Fragmented {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            let n = b.len().min(1);
            std::io::Read::read(&mut self.0, &mut b[..n])
        }
    }
    let mut wire = Vec::new();
    write_record(&mut wire, &json!({"v":1,"id":"\u{4e2d}\u{6587}"})).unwrap();
    write_record(&mut wire, &json!({"v":1,"id":"two"})).unwrap();
    let mut r = Fragmented(Cursor::new(wire));
    assert_eq!(
        read_record(&mut r).unwrap().unwrap()["id"],
        "\u{4e2d}\u{6587}"
    );
    assert_eq!(read_record(&mut r).unwrap().unwrap()["id"], "two");
    assert!(read_record(&mut r).unwrap().is_none());
    assert!(read_record(&mut Cursor::new(0u32.to_le_bytes())).is_err());
    assert!(read_record(&mut Cursor::new(4194305u32.to_le_bytes())).is_err());
    assert!(read_record(&mut Cursor::new(vec![1, 0])).is_err());
}

#[test]
fn default_encoder_is_decodable_constrained_baseline() -> Result<()> {
    use prtsd::{
        encoder::{Software, VideoEncoder},
        model::{Size, Target},
    };
    let target = Target::new(1, "about:blank".into(), Size::default(), true);
    let bgra = prtsd::video::render(&target, 1, 0.0);
    let mut encoder = Software::new(4_000_000, 30)?;
    let encoded = encoder.encode(&bgra, 1280, 720, true)?;
    let sps = encoded
        .windows(7)
        .find(|b| b[..3] == [0, 0, 1] && b[3] & 31 == 7)
        .context("missing SPS")?;
    assert_eq!(sps[4], 66, "H264 baseline profile");
    assert_ne!(
        sps[5] & 0x40,
        0,
        "constraint_set1 must select constrained baseline"
    );
    let mut decoder = Decoder::new()?;
    let picture = decoder.decode(&encoded)?.context("first IDR must decode")?;
    assert_eq!(picture.dimensions(), (1280, 720));
    Ok(())
}

#[tokio::test]
async fn malformed_connections_and_eof_cleanup() -> Result<()> {
    for mode in ["duplicate", "bad_version", "bad_length", "eof"] {
        let (c, mut child) = Control::start().await?;
        c.ok("hello", json!({"versions":[1]})).await?;
        c.ok("instance.create", json!({"options":{}})).await?;
        c.wait_event("instance.ready").await?;
        {
            let mut input = c.input.lock().await;
            match mode {
                "bad_length" => {
                    input
                        .as_mut()
                        .unwrap()
                        .write_all(&0u32.to_le_bytes())
                        .await?
                }
                "eof" => {
                    input.take();
                }
                _ => {
                    let value = if mode == "duplicate" {
                        json!({"v":1,"id":"r0","method":"instance.list","params":{}})
                    } else {
                        json!({"v":2,"id":"bad","method":"instance.list","params":{}})
                    };
                    let data = serde_json::to_vec(&value)?;
                    let w = input.as_mut().unwrap();
                    w.write_u32_le(data.len() as u32).await?;
                    w.write_all(&data).await?;
                }
            }
        }
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
        assert_eq!(status.success(), mode == "eof", "{mode}");
        assert!(
            c.log
                .lock()
                .unwrap()
                .iter()
                .any(|v| v["event"] == "instance.closed"),
            "{mode}: must close resources"
        );
    }
    Ok(())
}
