//! WebRTC adapter. The actor owns signaling; media workers see only bounded latest-frame slots.
use crate::model::{Error, Result, invalid};
use rtc::{
    interceptor::{Interceptor, Packet, Registry, Slot, StreamInfo, TaggedPacket},
    media::Sample,
    media_stream::MediaStreamTrack,
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
        RtpCodecKind,
    },
    sansio::Protocol,
    shared::marshal::MarshalSize,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinHandle};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit},
    media_stream::track_local::{TrackLocal, static_sample::TrackLocalStaticSample},
    peer_connection::{
        MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
        RTCIceCandidateInit, RTCIceGatheringState, RTCPeerConnectionIceEvent,
        RTCPeerConnectionState, RTCSessionDescription, register_default_interceptors,
    },
};

pub enum Event {
    Ice(String, Option<RTCIceCandidateInit>),
    State(String, RTCPeerConnectionState),
    Open(String),
    Message(String, bool, Value),
    Abuse(String),
    Closed(String),
}
#[derive(Clone)]
struct Handler {
    id: String,
    tx: mpsc::Sender<Event>,
    connected: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, e: RTCPeerConnectionIceEvent) {
        if let Ok(c) = e.candidate.to_json() {
            let _ = self.tx.send(Event::Ice(self.id.clone(), Some(c))).await;
        }
    }
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.tx.send(Event::Ice(self.id.clone(), None)).await;
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        self.connected
            .store(s == RTCPeerConnectionState::Connected, Ordering::Release);
        let _ = self.tx.send(Event::State(self.id.clone(), s)).await;
    }
}
pub struct Session {
    pub instance_id: String,
    pub control: bool,
    pub pc: Arc<dyn PeerConnection>,
    pub channel: Arc<dyn DataChannel>,
    pub awaiting: bool,
    pub answered: bool,
    pub pending: Vec<RTCIceCandidateInit>,
    pub remote_ufrag: Option<String>,
    pub local_ufrag: Option<String>,
    pub mid: Option<String>,
    pub sequence: u64,
    pub feedback_seq: u64,
    pub feedback: Option<Value>,
    pub open: bool,
    pub connected: bool,
    pub bytes: Arc<AtomicU64>,
    pub congestion: Arc<AtomicU64>,
    pub tasks: Vec<JoinHandle<()>>,
    pub failed: AtomicBool,
    pub errors: u32,
    pub last_stale_error: Option<Instant>,
}
impl Session {
    pub async fn create(
        id: String,
        instance_id: String,
        control: bool,
        tx: mpsc::Sender<Event>,
        video: &crate::video::Video,
    ) -> Result<Self> {
        let mut frames = video.frames.clone();
        let state = video.state.clone();
        let metrics = video.metrics.clone();
        let keyframe = video.keyframe.clone();
        let codec = RTCRtpCodec {
            mime_type: "video/H264".into(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                .into(),
            rtcp_feedback: vec![
                rtc::rtp_transceiver::rtp_sender::RTCPFeedback {
                    typ: "nack".into(),
                    parameter: "pli".into(),
                },
                rtc::rtp_transceiver::rtp_sender::RTCPFeedback {
                    typ: "ccm".into(),
                    parameter: "fir".into(),
                },
                rtc::rtp_transceiver::rtp_sender::RTCPFeedback {
                    typ: "goog-remb".into(),
                    parameter: String::new(),
                },
            ],
        };
        let mut engine = MediaEngine::default();
        engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: codec.clone(),
                payload_type: 102,
            },
            RtpCodecKind::Video,
        )?;
        let bytes = Arc::new(AtomicU64::new(0));
        let congestion = Arc::new(AtomicU64::new(0));
        // Closest to wire: count retransmissions as well. No frame delays masquerading as packet shaping.
        let registry = register_default_interceptors(Registry::new(), &mut engine)?.with(
            Slot::from(0),
            Telemetry {
                reads: VecDeque::new(),
                writes: VecDeque::new(),
                bytes: bytes.clone(),
                congestion: congestion.clone(),
                keyframe: keyframe.clone(),
            },
        );
        let connected = Arc::new(AtomicBool::new(false));
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_media_engine(engine)
                .with_interceptor_registry(registry)
                .with_handler(Arc::new(Handler {
                    id: id.clone(),
                    tx: tx.clone(),
                    connected: connected.clone(),
                }))
                .with_data_channel_send_buffer_limit(256 * 1024)
                .with_udp_addrs(vec!["0.0.0.0:0"])
                .build()
                .await?,
        );
        let ssrc = 0x50525453;
        let track = Arc::new(TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "prts".into(),
                "video".into(),
                "synthetic".into(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec,
                    ..Default::default()
                }],
            ),
        )?);
        let sender = pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
        let channel = pc
            .create_data_channel(
                "prts.control.v1",
                Some(RTCDataChannelInit {
                    ordered: true,
                    protocol: "prts-json-v1".into(),
                    ..Default::default()
                }),
            )
            .await?;
        let feedback = pc
            .create_data_channel(
                "prts.feedback.v1",
                Some(RTCDataChannelInit {
                    ordered: false,
                    max_retransmits: Some(0),
                    protocol: "prts-json-v1".into(),
                    ..Default::default()
                }),
            )
            .await?;
        let mut tasks = Vec::new();
        for (dc, is_feedback) in [(channel.clone(), false), (feedback, true)] {
            let (id, tx) = (id.clone(), tx.clone());
            tasks.push(tokio::spawn(async move {
                while let Some(event) = dc.poll().await {
                    let event = match event {
                        DataChannelEvent::OnOpen if !is_feedback => Some(Event::Open(id.clone())),
                        DataChannelEvent::OnMessage(msg) => {
                            if !msg.is_string || msg.data.len() > 65536 {
                                Some(Event::Abuse(id.clone()))
                            } else {
                                match serde_json::from_slice::<Value>(&msg.data) {
                                    Ok(v) if v.is_object() && v["v"] == 1 => {
                                        Some(Event::Message(id.clone(), is_feedback, v))
                                    }
                                    _ => Some(Event::Abuse(id.clone())),
                                }
                            }
                        }
                        DataChannelEvent::OnClose => {
                            let _ = tx.send(Event::Closed(id.clone())).await;
                            break;
                        }
                        _ => None,
                    };
                    if let Some(event) = event
                        && tx.send(event).await.is_err()
                    {
                        break;
                    }
                }
            }));
        }
        tasks.push(tokio::spawn(async move {
            let mut previous = 0;
            while frames.changed().await.is_ok() {
                let frame = frames.borrow_and_update().clone();
                let Some(frame) = frame else {
                    continue;
                };
                if !connected.load(Ordering::Acquire) {
                    continue;
                }
                if state.lock().unwrap().epoch != frame.epoch {
                    continue;
                }
                let parameters = match sender.get_parameters().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let pt = parameters
                    .rtp_parameters
                    .codecs
                    .iter()
                    .find(|c| c.rtp_codec.mime_type.eq_ignore_ascii_case("video/H264"))
                    .map(|c| c.payload_type)
                    .unwrap_or(102);
                if previous != 0 && frame.id > previous + 1 {
                    keyframe.store(true, Ordering::Release);
                }
                previous = frame.id;
                let sample = Sample {
                    data: frame.data,
                    duration: frame.duration,
                    ..Sample::new(frame.capture)
                };
                if tokio::time::timeout(
                    Duration::from_millis(100),
                    track.write_sample(ssrc, pt, &sample, &[]),
                )
                .await
                .is_ok_and(|r| r.is_ok())
                {
                    let mut m = metrics.lock().unwrap();
                    if m.send_ms.len() < 1200 {
                        m.send_ms
                            .push(frame.capture.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
        }));
        Ok(Self {
            instance_id,
            control,
            pc,
            channel,
            awaiting: false,
            answered: false,
            pending: Vec::new(),
            remote_ufrag: None,
            local_ufrag: None,
            mid: None,
            sequence: 0,
            feedback_seq: 0,
            feedback: None,
            open: false,
            connected: false,
            bytes,
            congestion,
            tasks,
            failed: AtomicBool::new(false),
            errors: 0,
            last_stale_error: None,
        })
    }
    pub async fn send(&self, v: Value) {
        if self.open && self.channel.try_send_text(&v.to_string()).await.is_err() {
            self.failed.store(true, Ordering::Release);
        }
    }
    pub async fn offer(&mut self, restart: bool) -> Result<Value> {
        if self.awaiting {
            return Err(Error::new(
                "invalid_state",
                "an offer is already outstanding",
            ));
        }
        let offer = self
            .pc
            .create_offer(Some(rtc::peer_connection::configuration::RTCOfferOptions {
                ice_restart: restart,
            }))
            .await?;
        self.local_ufrag = sdp_field(&offer.sdp, "a=ice-ufrag:");
        self.mid = sdp_field(&offer.sdp, "a=mid:");
        self.pc.set_local_description(offer.clone()).await?;
        self.awaiting = true;
        Ok(json!({"description":offer}))
    }
    pub async fn answer(&mut self, v: &Value) -> Result<()> {
        if !self.awaiting {
            return Err(Error::new(
                "invalid_state",
                "answer requires an outstanding offer",
            ));
        }
        if v["type"] != "answer" || v["sdp"].as_str().is_none_or(|s| s.len() > 1024 * 1024) {
            return Err(invalid("expected answer SDP (at most 1 MiB)"));
        }
        let desc: RTCSessionDescription =
            serde_json::from_value(v.clone()).map_err(|_| invalid("invalid description"))?;
        let ufrag = sdp_field(&desc.sdp, "a=ice-ufrag:")
            .ok_or_else(|| invalid("answer has no ICE ufrag"))?;
        self.pc.set_remote_description(desc).await?;
        self.remote_ufrag = Some(ufrag);
        self.awaiting = false;
        self.answered = true;
        let pending = std::mem::take(&mut self.pending);
        for candidate in pending {
            self.apply_ice(candidate).await?;
        }
        Ok(())
    }
    async fn apply_ice(&self, c: RTCIceCandidateInit) -> Result<()> {
        if !c.candidate.is_empty() && c.username_fragment != self.remote_ufrag {
            return Ok(());
        }
        self.pc.add_ice_candidate(c).await?;
        Ok(())
    }
    pub async fn ice(&mut self, v: &Value) -> Result<()> {
        let c = if v.is_null() {
            RTCIceCandidateInit::default()
        } else {
            let c: RTCIceCandidateInit =
                serde_json::from_value(v.clone()).map_err(|_| invalid("invalid ICE candidate"))?;
            if c.candidate.len() > 65536
                || c.username_fragment
                    .as_ref()
                    .is_none_or(|s| s.is_empty() || s.len() > 256)
                || (c.sdp_mid.is_none() && c.sdp_mline_index.is_none())
            {
                return Err(invalid("ICE requires usernameFragment and mid/index"));
            }
            c
        };
        if !self.answered || self.awaiting {
            if self.pending.len() >= 256 {
                return Err(Error::new("resource_exhausted", "remote ICE queue full"));
            }
            self.pending.push(c);
            Ok(())
        } else {
            self.apply_ice(c).await
        }
    }
    pub async fn close(mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), self.pc.close()).await;
    }
}
pub fn sdp_field(s: &str, key: &str) -> Option<String> {
    s.lines()
        .find_map(|line| line.trim_end().strip_prefix(key).map(str::to_owned))
}

struct Telemetry {
    reads: VecDeque<TaggedPacket>,
    writes: VecDeque<TaggedPacket>,
    bytes: Arc<AtomicU64>,
    congestion: Arc<AtomicU64>,
    keyframe: Arc<AtomicBool>,
}
impl Protocol<TaggedPacket, TaggedPacket, ()> for Telemetry {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = webrtc::error::Error;
    type Time = Instant;
    fn handle_read(&mut self, msg: TaggedPacket) -> std::result::Result<(), Self::Error> {
        if let Packet::Rtcp(ref packets) = msg.message.packet {
            for p in packets {
                if p.as_any().is::<rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication>() || p.as_any().is::<rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest>() {self.keyframe.store(true,Ordering::Release);}
                if let Some(remb)=p.as_any().downcast_ref::<rtc::rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate>() {self.congestion.store(remb.bitrate.max(1.0) as u64,Ordering::Release);}
            }
        }
        self.reads.push_back(msg);
        Ok(())
    }
    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.reads.pop_front()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> std::result::Result<(), Self::Error> {
        if let Packet::Rtp(ref p) = msg.message.packet {
            self.bytes
                .fetch_add(p.marshal_size() as u64, Ordering::Relaxed);
        }
        self.writes.push_back(msg);
        Ok(())
    }
    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.writes.pop_front()
    }
}
impl Interceptor for Telemetry {
    fn bind_local_stream(&mut self, _: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _: &StreamInfo) {}
}
