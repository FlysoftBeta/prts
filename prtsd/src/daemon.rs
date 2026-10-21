use crate::{
    model::*,
    transport::{self, Output},
    video::{Metrics, Video},
    viewer::{Event, Session},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use webrtc::peer_connection::RTCPeerConnectionState;

#[cfg(windows)]
type BrowserStartup = anyhow::Result<(
    std::sync::Arc<crate::browser::Browser>,
    crate::browser_wire::Target,
)>;

struct Instance {
    id: String,
    options: Options,
    state: &'static str,
    targets: BTreeMap<u64, Target>,
    next_target: u64,
    network: Network,
    video: Video,
    #[cfg(windows)]
    browser: Option<std::sync::Arc<crate::browser::Browser>>,
    #[cfg(windows)]
    startup: Option<tokio::task::JoinHandle<BrowserStartup>>,
    input_sequence: u64,
    startup_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Instance {
    fn release_input(&mut self) {
        #[cfg(windows)]
        if let Some(browser) = &self.browser
            && let Some((target, generation)) = self
                .active()
                .map(|t| (t.target_id.parse().unwrap(), t.generation.parse().unwrap()))
        {
            self.input_sequence += 1;
            let _ = browser.input(
                &crate::browser_wire::InputHeader {
                    target,
                    generation,
                    sequence: self.input_sequence,
                    modifiers: 0,
                },
                &crate::browser_wire::Input::Focus(false),
            );
        }
        for target in self.targets.values_mut() {
            target.input.release();
        }
    }
    fn active(&self) -> Option<&Target> {
        self.targets.values().find(|t| t.active)
    }
    fn snapshot(&self) -> Value {
        json!({"instance_id":self.id,"source":self.options.source,"state":self.state,"active_target_id":self.active().map(|t|&t.target_id),"options":self.options})
    }
    fn sync(&self, switch: bool) {
        let mut s = self.video.state.lock().unwrap();
        s.target = self.active().cloned();
        if switch {
            s.epoch += 1;
            s.keyframe = true;
        }
    }
    fn activate(&mut self, id: u64) -> Vec<Target> {
        let mut changed = Vec::new();
        for (tid, target) in &mut self.targets {
            if target.active && *tid != id {
                target.active = false;
                target.input.release();
                changed.push(target.clone());
            }
        }
        let target = self.targets.get_mut(&id).unwrap();
        if !target.active {
            target.active = true;
            target.bump();
            changed.push(target.clone());
        }
        changed
    }
}
struct Daemon {
    out: Output,
    instances: BTreeMap<String, Instance>,
    sessions: BTreeMap<String, Session>,
    next_instance: u64,
    next_session: u64,
    interval: u64,
    last_stats: Instant,
    tx: mpsc::Sender<Event>,
}
impl Daemon {
    fn instance(&self, p: &Value) -> Result<&Instance> {
        let id = string(p, "instance_id")?;
        let i = self
            .instances
            .get(id)
            .ok_or_else(|| Error::new("not_found", "Unknown instance"))?;
        if i.state != "ready" {
            return Err(Error::new("invalid_state", "instance is not ready"));
        }
        Ok(i)
    }
    fn target_id(&self, p: &Value) -> Result<u64> {
        let id = string(p, "target_id")?;
        let n = id
            .parse::<u64>()
            .map_err(|_| invalid("target_id must be a nonzero decimal string"))?;
        if n == 0 || n.to_string() != id {
            return Err(invalid(
                "target_id must be a canonical nonzero decimal string",
            ));
        }
        if !self.instance(p)?.targets.contains_key(&n) {
            return Err(Error::new("not_found", "Unknown target"));
        }
        Ok(n)
    }
    fn target_event(&self, name: &str, id: &str, t: &Target) {
        self.out.event(name, json!({"instance_id":id,"target":t}));
    }
    async fn broadcast(&self, id: &str) {
        let t = self.instances.get(id).and_then(Instance::active);
        for session in self.sessions.values().filter(|s| s.instance_id == id) {
            session
                .send(json!({"v":1,"type":"target","target":t}))
                .await;
        }
    }
    async fn close_session(&mut self, id: &str, reason: &str) {
        if let Some(s) = self.sessions.remove(id) {
            let iid = s.instance_id.clone();
            let control = s.control;
            s.close().await;
            if control && let Some(i) = self.instances.get_mut(&iid) {
                i.release_input();
                i.sync(false);
            }
            self.out.event(
                "session.closed",
                json!({"instance_id":iid,"session_id":id,"reason":reason}),
            );
            self.update_viewers();
        }
    }
    async fn close_instance(&mut self, id: &str, reason: &str) {
        if let Some(i) = self.instances.get_mut(id) {
            i.startup_cancel.store(true, Ordering::Release);
            i.state = "closing";
            i.video.stop();
        }
        let sessions: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.instance_id == id)
            .map(|(id, _)| id.clone())
            .collect();
        for sid in sessions {
            self.close_session(&sid, reason).await;
        }
        if let Some(mut i) = self.instances.remove(id) {
            i.video.join().await;
            #[cfg(windows)]
            {
                i.video.browser.lock().unwrap().take();
                if let Some(browser) = i.browser.take() {
                    let _ = tokio::task::spawn_blocking(move || browser.shutdown()).await;
                }
            }
            for t in i.targets.values() {
                self.out.event(
                    "target.closed",
                    json!({"instance_id":id,"target_id":t.target_id,"reason":reason}),
                );
            }
            self.out
                .event("instance.closed", json!({"instance_id":id,"reason":reason}));
        }
    }
    fn update_viewers(&self) {
        for i in self.instances.values() {
            let mut s = i.video.state.lock().unwrap();
            let count = self
                .sessions
                .values()
                .filter(|s| s.instance_id == i.id && s.connected)
                .count();
            if count > s.viewers {
                s.keyframe = true;
            }
            s.viewers = count;
            let congestion = self
                .sessions
                .values()
                .filter(|s| s.instance_id == i.id && s.connected)
                .map(|s| s.congestion.load(Ordering::Acquire))
                .filter(|n| *n > 0)
                .min()
                .unwrap_or(u64::MAX);
            let bps = (i.options.bitrate_caps.max_bps as u64)
                .min(i.network.bitrate_cap_bps.unwrap_or(u32::MAX) as u64)
                .min(congestion) as u32;
            // Small feedback fluctuations must not reinitialize the encoder on every actor tick.
            if bps < s.bitrate || bps as f64 > s.bitrate as f64 * 1.15 {
                s.bitrate = bps.max(1);
            }
        }
    }
    async fn command(&mut self, method: &str, p: &Value) -> Result<Value> {
        #[cfg(windows)]
        if method.starts_with("target.")
            && method != "target.list"
            && self.instance(p)?.options.source == "chromium"
        {
            return self.browser_command(method, p).await;
        }
        match method {
            "hello" => Err(Error::new("invalid_state", "hello already completed")),
            "instance.create" => {
                if self.instances.len() >= 8 {
                    return Err(Error::new("resource_exhausted", "at most eight instances"));
                }
                let options = Options::parse(
                    p.get("options")
                        .ok_or_else(|| invalid("options required"))?,
                )?;
                self.next_instance += 1;
                let id = format!("i{}", self.next_instance);
                let startup_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let i = Instance {
                    id: id.clone(),
                    video: Video::start(&options),
                    options: options.clone(),
                    state: "starting",
                    targets: BTreeMap::new(),
                    next_target: 1,
                    network: Network::default(),
                    #[cfg(windows)]
                    browser: None,
                    #[cfg(windows)]
                    startup: if options.source == "chromium" {
                        let options = options.clone();
                        let cancel = startup_cancel.clone();
                        Some(tokio::task::spawn_blocking(move || {
                            let path = std::env::var("PRTS_CHROME_PATH")
                                .unwrap_or_else(|_| crate::browser::DEFAULT_CHROME.into());
                            let browser = std::sync::Arc::new(
                                crate::browser::Browser::launch_cancellable(path, &cancel)?,
                            );
                            let target = browser.create(
                                &options.initial_url,
                                browser_size(options.size),
                                true,
                            )?;
                            browser.drain_events();
                            Ok((browser, target))
                        }))
                    } else {
                        None
                    },
                    input_sequence: 0,
                    startup_cancel,
                };
                let result = json!({"instance":i.snapshot()});
                self.instances.insert(id, i);
                Ok(result)
            }
            "instance.list" => Ok(
                json!({"instances":self.instances.values().map(Instance::snapshot).collect::<Vec<_>>()}),
            ),
            "instance.close" => {
                let id = string(p, "instance_id")?;
                if !self.instances.contains_key(id) {
                    return Err(Error::new("not_found", "Unknown instance"));
                }
                self.close_instance(id, "requested").await;
                Ok(json!({"closed":true}))
            }
            "target.list" => {
                Ok(json!({"targets":self.instance(p)?.targets.values().collect::<Vec<_>>()}))
            }
            "target.create" => {
                let i = self.instance(p)?;
                if i.targets.len() >= 256 {
                    return Err(Error::new("resource_exhausted", "at most 256 targets"));
                }
                let id = i.id.clone();
                let size = p
                    .get("size")
                    .map(Size::parse)
                    .transpose()?
                    .unwrap_or(i.options.size);
                let url = url(p
                    .get("url")
                    .map_or(Ok("about:blank"), |_| string(p, "url"))?)?;
                let activate = boolean(p, "activate", true)?;
                let i = self.instances.get_mut(&id).unwrap();
                let tid = i.next_target;
                i.next_target += 1;
                let target = Target::new(tid, url, size, i.targets.is_empty());
                i.targets.insert(tid, target.clone());
                self.target_event("target.created", &id, &target);
                let changes = if activate {
                    self.instances.get_mut(&id).unwrap().activate(tid)
                } else {
                    Vec::new()
                };
                for t in changes {
                    self.target_event("target.changed", &id, &t);
                }
                self.instances[&id].sync(true);
                self.broadcast(&id).await;
                Ok(json!({"target":self.instances[&id].targets[&tid]}))
            }
            "target.navigate" | "target.resize" | "target.activate" | "target.close" => {
                let tid = self.target_id(p)?;
                let id = self.instance(p)?.id.clone();
                let result = match method {
                    "target.navigate" => {
                        let url = url(string(p, "url")?)?;
                        let t = self
                            .instances
                            .get_mut(&id)
                            .unwrap()
                            .targets
                            .get_mut(&tid)
                            .unwrap();
                        t.url = url;
                        t.title = format!("PRTS synthetic: {}", t.url);
                        t.load = "loading";
                        t.complete_at = Some(Instant::now() + Duration::from_millis(15));
                        let t = t.clone();
                        self.target_event("target.changed", &id, &t);
                        json!({"accepted":true})
                    }
                    "target.resize" => {
                        let size =
                            Size::parse(p.get("size").ok_or_else(|| invalid("size required"))?)?;
                        let t = self
                            .instances
                            .get_mut(&id)
                            .unwrap()
                            .targets
                            .get_mut(&tid)
                            .unwrap();
                        t.size = size;
                        t.bump();
                        let t = t.clone();
                        self.target_event("target.changed", &id, &t);
                        json!({"target":t})
                    }
                    "target.activate" => {
                        let changes = self.instances.get_mut(&id).unwrap().activate(tid);
                        for t in changes {
                            self.target_event("target.changed", &id, &t);
                        }
                        json!({"target":self.instances[&id].targets[&tid]})
                    }
                    _ => {
                        let i = self.instances.get_mut(&id).unwrap();
                        let removed = i.targets.remove(&tid).unwrap();
                        let next = if removed.active {
                            i.targets.keys().next().copied()
                        } else {
                            None
                        };
                        self.out.event("target.closed",json!({"instance_id":id,"target_id":removed.target_id,"reason":"requested"}));
                        if let Some(next) = next {
                            let changes = self.instances.get_mut(&id).unwrap().activate(next);
                            for t in changes {
                                self.target_event("target.changed", &id, &t);
                            }
                        }
                        json!({"closed":true})
                    }
                };
                self.instances[&id].sync(method != "target.navigate");
                self.broadcast(&id).await;
                Ok(result)
            }
            "session.create" => {
                let i = self.instance(p)?;
                let id = i.id.clone();
                let viewer = string(p, "viewer_id")?;
                if viewer.is_empty() || viewer.len() > 128 {
                    return Err(invalid("viewer_id must be 1..128 bytes"));
                }
                let control = boolean(p, "control", false)?;
                if self
                    .sessions
                    .values()
                    .any(|s| s.instance_id == id && s.control && control)
                {
                    return Err(Error::new(
                        "permission_denied",
                        "another viewer owns control",
                    ));
                }
                if self.sessions.len() >= 32 {
                    return Err(Error::new("resource_exhausted", "at most 32 sessions"));
                }
                self.next_session += 1;
                let sid = format!("s{}", self.next_session);
                let i = &self.instances[&id];
                let session = Session::create(
                    sid.clone(),
                    id,
                    control,
                    self.tx.clone(),
                    &i.video,
                    &i.network,
                )
                .await?;
                self.sessions.insert(sid.clone(), session);
                Ok(json!({"session_id":sid,"polite":false,"ice_servers":[],"control":control}))
            }
            "session.offer" | "session.answer" | "session.ice" | "session.close" => {
                let sid = string(p, "session_id")?;
                let s = self
                    .sessions
                    .get_mut(sid)
                    .ok_or_else(|| Error::new("not_found", "Unknown session"))?;
                match method {
                    "session.offer" => s.offer(boolean(p, "ice_restart", false)?).await,
                    "session.answer" => {
                        s.answer(
                            p.get("description")
                                .ok_or_else(|| invalid("description required"))?,
                        )
                        .await?;
                        Ok(json!({"accepted":true}))
                    }
                    "session.ice" => {
                        s.ice(
                            p.get("candidate")
                                .ok_or_else(|| invalid("candidate required"))?,
                        )
                        .await?;
                        Ok(json!({"accepted":true}))
                    }
                    _ => {
                        self.close_session(sid, "requested").await;
                        Ok(json!({"closed":true}))
                    }
                }
            }
            "network.set" => {
                let id = self.instance(p)?.id.clone();
                let n = Network::parse(
                    p.get("settings")
                        .ok_or_else(|| invalid("settings required"))?,
                )?;
                let result = json!({"settings":n});
                for session in self.sessions.values().filter(|s| s.instance_id == id) {
                    session.shaper.configure(&n);
                    // A changed simulated link invalidates the previous link's
                    // estimate; allow fresh feedback instead of pinning a
                    // restored connection to its old low-cap REMB forever.
                    session.congestion.store(0, Ordering::Release);
                }
                self.instances.get_mut(&id).unwrap().network = n;
                self.update_viewers();
                Ok(result)
            }
            "stats.configure" => {
                let interval = integer(p, "interval_ms", 0, 10000)?;
                if interval != 0 && interval < 250 {
                    return Err(invalid("interval_ms must be zero or 250..10000"));
                }
                self.interval = interval;
                self.last_stats = Instant::now();
                self.reset_stats();
                Ok(json!({"interval_ms":interval}))
            }
            _ => Err(Error::new("unsupported", "Unknown method")),
        }
    }
    fn reset_stats(&self) {
        for i in self.instances.values() {
            let mut m = i.video.metrics.lock().unwrap();
            let failure = m.failure.take();
            let warnings = std::mem::take(&mut m.warnings);
            *m = Metrics {
                failure,
                warnings,
                ..Default::default()
            };
        }
        for s in self.sessions.values() {
            s.bytes.store(0, Ordering::Release);
        }
    }
    async fn tick(&mut self) {
        let mut targets = Vec::new();
        let mut ready = Vec::new();
        let mut failed = Vec::new();
        for i in self.instances.values_mut() {
            #[cfg(windows)]
            if i.state == "starting" && i.startup.as_ref().is_some_and(|s| s.is_finished()) {
                match i.startup.take().unwrap().await {
                    Ok(Ok((browser, target))) => {
                        *i.video.browser.lock().unwrap() = Some(browser.clone());
                        i.browser = Some(browser);
                        i.targets.insert(target.id, browser_target(target.clone()));
                        i.state = "ready";
                        targets.push((i.id.clone(), "target.created", browser_target(target)));
                        ready.push(i.id.clone());
                        i.sync(true);
                    }
                    Ok(Err(e)) => {
                        failed.push((i.id.clone(), format!("Browser startup failed: {e:#}")))
                    }
                    Err(e) => failed.push((i.id.clone(), format!("Browser startup task: {e}"))),
                }
            }
            #[cfg(windows)]
            if let Some(browser) = &i.browser {
                for event in browser.drain_events() {
                    use crate::browser_wire::Event as B;
                    match event {
                        B::Created(t) | B::Changed(t) => {
                            let name = if i.targets.contains_key(&t.id) {
                                "target.changed"
                            } else {
                                "target.created"
                            };
                            let mut t = browser_target(t);
                            if let Some(old) = i.targets.get(&t.target_id.parse().unwrap())
                                && old.generation == t.generation
                                && old.active == t.active
                            {
                                t.input = old.input.clone();
                            }
                            let changed =
                                i.targets
                                    .get(&t.target_id.parse().unwrap())
                                    .is_none_or(|old| {
                                        old.generation != t.generation || old.active != t.active
                                    });
                            i.targets.insert(t.target_id.parse().unwrap(), t.clone());
                            i.sync(changed);
                            targets.push((i.id.clone(), name, t));
                        }
                        B::Closed { target, reason } => {
                            i.targets.remove(&target);
                            i.sync(true);
                            self.out.event("target.closed", json!({"instance_id":i.id,"target_id":target.to_string(),"reason":reason}));
                        }
                        B::Cursor {
                            target,
                            generation,
                            cursor,
                        } => {
                            let shape = [
                                "default",
                                "pointer",
                                "text",
                                "crosshair",
                                "move",
                                "ew-resize",
                                "ns-resize",
                                "none",
                            ];
                            let cursor = json!({"shape":shape.get(cursor.shape as usize).unwrap_or(&"default")});
                            self.out.event("cursor.changed", json!({"instance_id":i.id,"target_id":target.to_string(),"generation":generation.to_string(),"cursor":cursor}));
                            for s in self.sessions.values().filter(|s| s.instance_id == i.id) {
                                s.send(json!({"v":1,"type":"cursor","target_id":target.to_string(),"generation":generation.to_string(),"cursor":cursor})).await;
                            }
                        }
                        B::Fault {
                            fatal: true,
                            message,
                            ..
                        } => failed.push((i.id.clone(), message)),
                        _ => {}
                    }
                }
            }
            if i.options.source == "synthetic"
                && i.state == "starting"
                && *i.video.encoder_name.lock().unwrap() != "initializing"
            {
                let target = Target::new(1, i.options.initial_url.clone(), i.options.size, true);
                i.targets.insert(1, target.clone());
                i.next_target = 2;
                i.state = "ready";
                targets.push((i.id.clone(), "target.created", target));
                ready.push(i.id.clone());
                i.sync(true);
            }
            for t in i.targets.values_mut() {
                if t.complete_at.is_some_and(|at| at <= Instant::now()) {
                    t.load = "complete";
                    t.complete_at = None;
                    targets.push((i.id.clone(), "target.changed", t.clone()));
                }
            }
            let error = i.video.metrics.lock().unwrap().failure.take();
            for warning in std::mem::take(&mut i.video.metrics.lock().unwrap().warnings) {
                eprintln!("prtsd: {warning}");
                self.out.event(
                    "log",
                    json!({"level":"warn","instance_id":i.id,"message":warning}),
                );
            }
            if let Some(e) = error {
                failed.push((i.id.clone(), e));
            }
        }
        let changed_ids: std::collections::BTreeSet<_> =
            targets.iter().map(|(id, _, _)| id.clone()).collect();
        for (id, event, target) in targets {
            self.target_event(event, &id, &target);
            self.instances[&id].sync(false);
        }
        for id in changed_ids {
            self.broadcast(&id).await;
        }
        for id in ready {
            let i = &self.instances[&id];
            self.out.event("instance.ready",json!({"instance_id":id,"source":i.options.source,"active_target_id":i.active().map(|t| &t.target_id),"encoder":*i.video.encoder_name.lock().unwrap(),"isolation_profile":i.options.isolation_profile}));
        }
        for (id, error) in failed {
            if self.instances[&id].state == "failed" {
                continue;
            }
            eprintln!("prtsd: instance failed for {id}: {error}");
            let i = self.instances.get_mut(&id).unwrap();
            i.state = "failed";
            i.startup_cancel.store(true, Ordering::Release);
            i.video.stop();
            self.out.event(
                "instance.failed",
                json!({"instance_id":id,"error":Error::new("internal","instance source or encoder failed")}),
            );
            let sessions: Vec<_> = self
                .sessions
                .iter()
                .filter(|(_, s)| s.instance_id == id)
                .map(|(id, _)| id.clone())
                .collect();
            for sid in sessions {
                self.close_session(&sid, "instance_failed").await;
            }
            // A failed instance keeps its diagnostic ID, not a live browser
            // or imported resources. Joining completes outstanding CPU reads.
            let i = self.instances.get_mut(&id).unwrap();
            i.video.join().await;
            #[cfg(windows)]
            {
                i.video.browser.lock().unwrap().take();
                if let Some(browser) = i.browser.take() {
                    let _ = tokio::task::spawn_blocking(move || drop(browser)).await;
                }
            }
        }
        let failed_sessions: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.failed.load(Ordering::Acquire))
            .map(|(id, _)| id.clone())
            .collect();
        for id in failed_sessions {
            self.close_session(&id, "datachannel_backpressure").await;
        }
        self.update_viewers();
        if self.interval > 0 && self.last_stats.elapsed() >= Duration::from_millis(self.interval) {
            self.stats().await;
        }
    }
    async fn stats(&mut self) {
        let elapsed = self.last_stats.elapsed();
        self.last_stats = Instant::now();
        let seconds = elapsed.as_secs_f64();
        for i in self.instances.values() {
            if i.state != "ready" {
                continue;
            }
            let mut m = {
                let mut guard = i.video.metrics.lock().unwrap();
                let mut sample = std::mem::take(&mut *guard);
                // Lifecycle diagnostics belong to tick(), including failures that
                // arrive while stats awaits another viewer's transport report.
                guard.frame_backend = sample.frame_backend.clone();
                guard.encoder_input = sample.encoder_input.clone();
                guard.failure = sample.failure.take();
                guard.warnings = std::mem::take(&mut sample.warnings);
                sample
            };
            let avg = |v: &[f64]| {
                if v.is_empty() {
                    None
                } else {
                    Some(v.iter().sum::<f64>() / v.len() as f64)
                }
            };
            let encode_avg = avg(&m.encode_ms);
            let capture_encode_avg = avg(&m.capture_encode_ms);
            m.capture_encode_ms.sort_by(f64::total_cmp);
            let p95 = if m.capture_encode_ms.is_empty() {
                None
            } else {
                Some(
                    m.capture_encode_ms[((m.capture_encode_ms.len() as f64 * 0.95).ceil()
                        as usize
                        - 1)
                    .min(m.capture_encode_ms.len() - 1)],
                )
            };
            let mut sessions = Vec::new();
            for (id, s) in self.sessions.iter().filter(|(_, s)| s.instance_id == i.id) {
                let report =
                    s.pc.get_stats(Instant::now(), webrtc::peer_connection::StatsSelector::None)
                        .await;
                let rtt = report
                    .candidate_pairs()
                    .find(|p| p.nominated && p.responses_received > 0)
                    .map(|p| p.current_round_trip_time * 1000.0);
                let available = s.congestion.load(Ordering::Acquire);
                sessions.push(json!({"session_id":id,"rtt_ms":rtt,"available_outgoing_bps":if available==0 {None}else{Some(available)},"rtp_sent_bps":s.bytes.swap(0,Ordering::AcqRel) as f64*8.0/seconds,"shaper_dropped_packets":s.shaper.dropped.load(Ordering::Relaxed),"viewer_feedback":s.feedback}));
            }
            self.out.event("stats",json!({"instance_id":i.id,"interval_ms":elapsed.as_millis() as u64,
                "encoder":{"input_path":m.encoder_input,"name":*i.video.encoder_name.lock().unwrap(),"fps":m.encoded as f64/seconds,"bitrate_bps":m.bytes as f64*8.0/seconds,"qp_avg":null,"encode_ms_avg":encode_avg},
                "frames":{"backend":m.frame_backend,"captured":m.captured,"encoded":m.encoded,"dropped":m.dropped,"capture_to_encode_ms_avg":capture_encode_avg,"capture_to_encode_ms_p95":p95,"capture_to_send_ms_avg":avg(&m.send_ms)},"sessions":sessions}));
        }
    }
    async fn event(&mut self, event: Event) {
        match event {
            Event::Ice(id, candidate) => {
                if let Some(s) = self.sessions.get(&id) {
                    let candidate = candidate.map(|mut c| {
                        c.username_fragment = s.local_ufrag.clone();
                        c.sdp_mid = s.mid.clone();
                        c.sdp_mline_index = Some(0);
                        c
                    });
                    self.out.event(
                        "session.ice",
                        json!({"instance_id":s.instance_id,"session_id":id,"candidate":candidate}),
                    );
                }
            }
            Event::State(id, state) => {
                if matches!(
                    state,
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
                ) {
                    self.close_session(&id, "connection_failed").await;
                    return;
                }
                if let Some(s) = self.sessions.get_mut(&id) {
                    s.connected = state == RTCPeerConnectionState::Connected;
                    let name = match state {
                        RTCPeerConnectionState::Connected => "connected",
                        RTCPeerConnectionState::Disconnected => "disconnected",
                        _ => "connecting",
                    };
                    if !s.connected
                        && s.control
                        && let Some(i) = self.instances.get_mut(&s.instance_id)
                    {
                        i.release_input();
                        i.sync(false);
                    }
                    if state == RTCPeerConnectionState::Disconnected {
                        s.control = false;
                    }
                    self.out.event(
                        "session.state",
                        json!({"instance_id":s.instance_id,"session_id":id,"state":name}),
                    );
                }
                self.update_viewers();
            }
            Event::Open(id) => {
                if let Some(s) = self.sessions.get_mut(&id) {
                    s.open = true;
                    let target = self
                        .instances
                        .get(&s.instance_id)
                        .and_then(Instance::active);
                    s.send(json!({"v":1,"type":"ready","instance_id":s.instance_id,"session_id":id,"control":s.control,"target":target})).await;
                }
            }
            Event::Message(id, feedback, v) => {
                let iid = self.sessions.get(&id).map(|s| s.instance_id.clone());
                let before = iid
                    .as_ref()
                    .and_then(|id| self.instances.get(id))
                    .and_then(Instance::active)
                    .map(|t| t.input.pointer);
                let result = self.input(&id, feedback, &v);
                if let Err(error) = result {
                    if let Some(s) = self.sessions.get_mut(&id) {
                        if error.code == "stale_state" {
                            if s.last_stale_error
                                .is_some_and(|at| at.elapsed() < Duration::from_millis(200))
                            {
                                return;
                            }
                            s.last_stale_error = Some(Instant::now());
                        } else {
                            s.errors += 1;
                        }
                        if s.errors >= 16 {
                            self.close_session(&id, "repeated_invalid_input").await;
                            return;
                        }
                        s.send(json!({"v":1,"type":"error","sequence":v.get("sequence"),"error":error})).await;
                    }
                } else if let Some(iid) = iid
                    && let Some(t) = self.instances.get(&iid).and_then(Instance::active)
                    && before != Some(t.input.pointer)
                {
                    let cursor = json!({"v":1,"type":"cursor","target_id":t.target_id,"generation":t.generation,"cursor":{"shape":if t.input.pointer {"pointer"}else{"default"}}});
                    for s in self.sessions.values().filter(|s| s.instance_id == iid) {
                        s.send(cursor.clone()).await;
                    }
                }
            }
            Event::Abuse(id) => self.close_session(&id, "invalid_datachannel_message").await,
            Event::Closed(id) => self.close_session(&id, "datachannel_closed").await,
        }
    }
    fn input(&mut self, id: &str, feedback: bool, v: &Value) -> Result<()> {
        let Some(s) = self.sessions.get_mut(id) else {
            return Ok(());
        };
        let kind = string(v, "type")?;
        if kind == "stats" {
            if !feedback {
                return Err(invalid("stats must use feedback channel"));
            }
            let seq = integer(v, "sample_seq", 1, MAX_SAFE)?;
            if seq <= s.feedback_seq {
                return Ok(());
            }
            integer(v, "interval_ms", 1, 60000)?;
            number(v, "decoded_fps", 0.0, 1000.0)?;
            number(v, "received_bps", 0.0, 1_000_000_000.0)?;
            for key in ["rtt_ms", "jitter_ms", "decode_ms_avg", "render_ms_avg"] {
                if !v.get(key).is_some_and(Value::is_null) {
                    number(v, key, 0.0, 60000.0)?;
                }
            }
            let lost = v["packets_lost"]
                .as_i64()
                .filter(|n| n.unsigned_abs() <= MAX_SAFE);
            if lost.is_none() {
                return Err(invalid("invalid packets_lost"));
            }
            s.feedback_seq = seq;
            s.feedback = Some(v.clone());
            return Ok(());
        }
        if feedback {
            return Err(invalid("only stats is allowed on feedback channel"));
        }
        if !matches!(
            kind,
            "mouse.move"
                | "mouse.down"
                | "mouse.up"
                | "mouse.wheel"
                | "key.down"
                | "key.up"
                | "key.char"
                | "focus"
                | "touch.start"
                | "touch.move"
                | "touch.end"
                | "touch.cancel"
        ) {
            return Err(Error::new(
                "unsupported",
                "Unknown DataChannel message type",
            ));
        }
        if !s.control {
            return Err(Error::new("permission_denied", "view-only session"));
        }
        let seq = integer(v, "sequence", 1, MAX_SAFE)?;
        if seq <= s.sequence {
            return Ok(());
        }
        s.sequence = seq;
        integer(v, "modifiers", 0, 63)?;
        let i = self
            .instances
            .get_mut(&s.instance_id)
            .ok_or_else(|| Error::new("not_found", "Unknown instance"))?;
        let target = i
            .targets
            .values_mut()
            .find(|t| t.active)
            .ok_or_else(|| Error::new("stale_state", "no active target"))?;
        if string(v, "target_id")? != target.target_id
            || string(v, "generation")? != target.generation
        {
            return Err(Error::new(
                "stale_state",
                "input target or generation is stale",
            ));
        }
        let input = &mut target.input;
        if kind.starts_with("touch.") {
            if i.options.source != "chromium" {
                return Err(invalid("touch requires a Chromium window stream"));
            }
            #[cfg(windows)]
            if i.browser
                .as_ref()
                .is_none_or(|b| b.hello.features & crate::window_wire::TOUCH_INPUT == 0)
            {
                return Err(invalid("browser did not advertise touch support"));
            }
            let id = integer(v, "contact_id", 0, 31)? as u32;
            let x = number(v, "x", -32768., 32767.)?;
            let y = number(v, "y", -32768., 32767.)?;
            number(v, "pressure", 0., 1.)?;
            let timestamp = number(v, "timestamp_ms", 0., 1e12)?;
            if timestamp < input.touch_time_ms {
                return Err(invalid("touch timestamp decreased"));
            }
            if kind == "touch.start" {
                if input.touches.contains_key(&id) || input.touches.len() >= 10 {
                    return Err(invalid("touch contact already held or capacity exceeded"));
                }
                if x < 0.
                    || y < 0.
                    || x >= target.size.width as f64
                    || y >= target.size.height as f64
                {
                    return Err(invalid("touch start outside window"));
                }
                input.touches.insert(id, true);
            } else {
                if !input.touches.contains_key(&id) {
                    return Err(invalid("touch contact not held"));
                }
                if kind != "touch.move" {
                    input.touches.remove(&id);
                }
            }
            input.touch_time_ms = timestamp;
        } else if kind.starts_with("mouse.") {
            let x = number(v, "x", -32768.0, 32767.0)?;
            let y = number(v, "y", -32768.0, 32767.0)?;
            let buttons = integer(v, "buttons", 0, 31)? as u32;
            let outside = x < 0.0
                || y < 0.0
                || x >= target.size.width as f64
                || y >= target.size.height as f64;
            if outside && matches!(kind, "mouse.down" | "mouse.wheel") {
                return Ok(());
            }
            let mut next = input.buttons;
            if matches!(kind, "mouse.down" | "mouse.up") {
                let button = integer(v, "button", 1, 5)?;
                integer(v, "click_count", 1, 3)?;
                let mask = match button {
                    1 => 1,
                    2 => 4,
                    3 => 2,
                    4 => 8,
                    _ => 16,
                };
                if kind == "mouse.down" {
                    if next & mask != 0 {
                        return Err(invalid("button already held"));
                    }
                    next |= mask;
                } else {
                    if next & mask == 0 {
                        return Err(invalid("button not held"));
                    }
                    next &= !mask;
                }
            }
            if next != buttons {
                return Err(invalid("buttons do not match tracked state"));
            }
            if kind == "mouse.wheel" {
                number(v, "delta_x", -1_000_000.0, 1_000_000.0)?;
                input.wheel += number(v, "delta_y", -1_000_000.0, 1_000_000.0)?;
            }
            input.x = x;
            input.y = y;
            input.buttons = next;
            if kind == "mouse.down" {
                input.click = Some(Instant::now());
                input.focused = true;
            }
            let pointer = x >= target.size.width as f64 / 3.0
                && x < target.size.width as f64 * 2.0 / 3.0
                && y >= target.size.height as f64 / 3.0
                && y < target.size.height as f64 * 2.0 / 3.0;
            if i.options.source == "synthetic" && pointer != input.pointer {
                input.pointer = pointer;
                self.out.event("cursor.changed",json!({"instance_id":i.id,"target_id":target.target_id,"generation":target.generation,"cursor":{"shape":if pointer {"pointer"} else {"default"}}}));
                // Channel sends are performed by the actor after this synchronous validation.
            }
        } else if kind == "focus" {
            let focused = v
                .get("focused")
                .and_then(Value::as_bool)
                .ok_or_else(|| invalid("focused must be boolean"))?;
            input.focused = focused;
            if !focused {
                input.buttons = 0;
                input.keys.clear();
                input.touches.clear();
                input.touch_time_ms = 0.;
            }
        } else {
            let text = string(v, "text")?;
            if text.len() > 4096 || text.contains('\0') {
                return Err(invalid("text must be at most 4096 UTF-8 bytes without NUL"));
            }
            if kind == "key.char" {
                input.text.push_str(text);
                input.text = input
                    .text
                    .chars()
                    .rev()
                    .take(256)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
            } else {
                let code = string(v, "code")?;
                let key = string(v, "key")?;
                if code.len() > 128
                    || key.len() > 128
                    || (kind == "key.down"
                        && input.keys.len() >= 256
                        && !input.keys.contains_key(code))
                {
                    return Err(invalid("key/code or held-key capacity exceeded"));
                }
                integer(v, "location", 0, 3)?;
                let repeat = v
                    .get("repeat")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("repeat required"))?;
                v.get("is_system")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("is_system required"))?;
                if let Some(vk) = v.get("vk") {
                    if vk.as_u64().is_none_or(|n| n > 255) {
                        return Err(invalid("vk outside 0..255"));
                    }
                } else if virtual_key(code, key).is_none() {
                    return Err(invalid("key has no supported VK mapping"));
                }
                if code.is_empty() && key != "Unidentified" {
                    return Err(invalid("empty code requires Unidentified key"));
                }
                let held = input.keys.contains_key(code);
                if kind == "key.up" {
                    if repeat || !text.is_empty() || !held {
                        return Err(invalid("invalid key.up state"));
                    }
                    input.keys.remove(code);
                } else {
                    if held != repeat {
                        return Err(invalid("repeat does not match held key"));
                    }
                    input.keys.insert(code.into(), true);
                }
                input.last_key = key.into();
            }
        }
        #[cfg(windows)]
        if let Some(browser) = &i.browser {
            use crate::browser_wire::{Input as B, InputHeader, Key};
            let buttons = v["buttons"].as_u64().unwrap_or(0) as u32;
            let x = v["x"].as_f64().unwrap_or(0.);
            let y = v["y"].as_f64().unwrap_or(0.);
            let button = v["button"].as_u64().unwrap_or(0) as u8;
            let click_count = v["click_count"].as_u64().unwrap_or(1) as u8;
            let command = match kind {
                "touch.start" | "touch.move" | "touch.end" | "touch.cancel" => B::Touch {
                    phase: match kind {
                        "touch.start" => 0,
                        "touch.move" => 1,
                        "touch.end" => 2,
                        _ => 3,
                    },
                    id: v["contact_id"].as_u64().unwrap() as u32,
                    x,
                    y,
                    pressure: v["pressure"].as_f64().unwrap(),
                    timestamp_ms: v["timestamp_ms"].as_f64().unwrap(),
                },
                "mouse.move" => B::MouseMove { x, y, buttons },
                "mouse.down" => B::MouseDown {
                    x,
                    y,
                    button,
                    buttons,
                    click_count,
                },
                "mouse.up" => B::MouseUp {
                    x,
                    y,
                    button,
                    buttons,
                    click_count,
                },
                "mouse.wheel" => B::Wheel {
                    x,
                    y,
                    delta_x: v["delta_x"].as_f64().unwrap(),
                    delta_y: v["delta_y"].as_f64().unwrap(),
                    buttons,
                },
                "focus" => B::Focus(v["focused"].as_bool().unwrap()),
                "key.char" => B::Char(v["text"].as_str().unwrap().into()),
                _ => {
                    let code = v["code"].as_str().unwrap();
                    let key = v["key"].as_str().unwrap();
                    let k = Key {
                        vk: v["vk"]
                            .as_u64()
                            .map(|v| v as u32)
                            .or_else(|| virtual_key(code, key).map(u32::from))
                            .unwrap(),
                        code: code.into(),
                        key: key.into(),
                        text: v["text"].as_str().unwrap().into(),
                        location: v["location"].as_u64().unwrap() as u8,
                        repeat: v["repeat"].as_bool().unwrap(),
                        is_system: v["is_system"].as_bool().unwrap(),
                    };
                    if kind == "key.down" {
                        B::KeyDown(k)
                    } else {
                        B::KeyUp(k)
                    }
                }
            };
            i.input_sequence += 1;
            browser
                .input(
                    &InputHeader {
                        target: target.target_id.parse().unwrap(),
                        generation: target.generation.parse().unwrap(),
                        sequence: i.input_sequence,
                        modifiers: v["modifiers"].as_u64().unwrap() as u32,
                    },
                    &command,
                )
                .map_err(|e| Error::new("internal", e.to_string()))?;
        }
        i.sync(false);
        Ok(())
    }
}
fn virtual_key(code: &str, key: &str) -> Option<u8> {
    if code.starts_with("Key") && code.len() == 4 && code.as_bytes()[3].is_ascii_uppercase() {
        return Some(code.as_bytes()[3]);
    }
    if code.starts_with("Digit") && code.len() == 6 && code.as_bytes()[5].is_ascii_digit() {
        return Some(code.as_bytes()[5]);
    }
    if let Some(n) = code
        .strip_prefix('F')
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        return Some(0x6f + n);
    }
    if let Some(n) = code
        .strip_prefix("Numpad")
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|n| *n <= 9)
    {
        return Some(0x60 + n);
    }
    Some(match code {
        "ShiftLeft" => 0xa0,
        "ShiftRight" => 0xa1,
        "ControlLeft" => 0xa2,
        "ControlRight" => 0xa3,
        "AltLeft" => 0xa4,
        "AltRight" => 0xa5,
        "MetaLeft" => 0x5b,
        "MetaRight" => 0x5c,
        "Semicolon" => 0xba,
        "Equal" => 0xbb,
        "Comma" => 0xbc,
        "Minus" => 0xbd,
        "Period" => 0xbe,
        "Slash" => 0xbf,
        "Backquote" => 0xc0,
        "BracketLeft" => 0xdb,
        "Backslash" => 0xdc,
        "BracketRight" => 0xdd,
        "Quote" => 0xde,
        "NumpadMultiply" => 0x6a,
        "NumpadAdd" => 0x6b,
        "NumpadSubtract" => 0x6d,
        "NumpadDecimal" => 0x6e,
        "NumpadDivide" => 0x6f,
        "NumpadEnter" => 0x0d,
        _ => match key {
            "Enter" => 0x0d,
            "Tab" => 9,
            "Escape" => 0x1b,
            "Backspace" => 8,
            "Delete" => 0x2e,
            "Insert" => 0x2d,
            "ArrowLeft" => 0x25,
            "ArrowRight" => 0x27,
            "ArrowUp" => 0x26,
            "ArrowDown" => 0x28,
            "Shift" => 0x10,
            "Control" => 0x11,
            "Alt" => 0x12,
            "Meta" => 0x5b,
            " " => 0x20,
            "Home" => 0x24,
            "End" => 0x23,
            "PageUp" => 0x21,
            "PageDown" => 0x22,
            "CapsLock" => 0x14,
            "NumLock" => 0x90,
            "ScrollLock" => 0x91,
            "Pause" => 0x13,
            "PrintScreen" => 0x2c,
            "ContextMenu" => 0x5d,
            _ => return None,
        },
    })
}
pub async fn run() -> anyhow::Result<()> {
    #[cfg(windows)]
    eprintln!("prtsd: daemon pid {}", unsafe {
        windows::Win32::System::Threading::GetCurrentProcessId()
    });
    let mut input = transport::input();
    let out = Output::start();
    let (tx, mut events) = mpsc::channel(1024);
    let mut d = Daemon {
        out: out.clone(),
        instances: BTreeMap::new(),
        sessions: BTreeMap::new(),
        next_instance: 0,
        next_session: 0,
        interval: 1000,
        last_stats: Instant::now(),
        tx,
    };
    let mut ids = HashSet::new();
    let mut hello = false;
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result: anyhow::Result<()>=async {
        loop {
            if out.failed.load(Ordering::Acquire) {anyhow::bail!("stdout closed or output queue exceeded 8 MiB");}
            tokio::select! {
                record=input.recv()=>{
                    let Some(record)=record else {break;};let record=record?;
                    if record["v"]!=1 {anyhow::bail!("invalid protocol version");}
                    let id=record["id"].as_str().filter(|s| !s.is_empty() && s.len()<=128).ok_or_else(||anyhow::anyhow!("invalid request id"))?;
                    if !ids.insert(id.to_owned()) {anyhow::bail!("duplicate request id");}
                    // A finite connection budget prevents an unbounded uniqueness set.
                    if ids.len()>1_000_000 {anyhow::bail!("request ID lifetime capacity reached; reconnect");}
                    let method=record["method"].as_str();let params=&record["params"];
                    if !hello {
                        if method!=Some("hello") {anyhow::bail!("first request must be hello");}
                        if !params.is_object() || !params["versions"].as_array().is_some_and(|a| a.len()<=4096 && a.iter().all(|v|v.as_u64().is_some_and(|n|n<=MAX_SAFE)) && a.contains(&json!(1))) {
                            out.response(id,Err(Error::new("unsupported","protocol v1 required")));break;
                        }
                        out.response(id,Ok(json!({"version":1,"daemon":"prtsd","capabilities":{"sources":["synthetic","chromium"],"encoders":["auto","qsv","software"],"isolation_profiles":["job"],"max_instances":8,"max_sessions":32,"max_width":4096,"max_height":4096,"max_pixels":8294400,"max_device_width":4096,"max_device_height":4096,"min_device_dimension":16,"device_dimension_alignment":1,"fps":{"default":30,"min":1,"max":60},"packet_shaping":true,"bitrate_cap":true,"custom_cursor":false}})));hello=true;
                    } else {
                        let response=match method {
                            Some(method) if params.is_object()=>match tokio::time::timeout(Duration::from_secs(5),d.command(method,params)).await {Ok(r)=>r,Err(_)=>{out.failed.store(true,Ordering::Release);Err(Error::new("timeout","command exceeded five seconds; daemon shutting down"))}},
                            _=>Err(invalid("method string and params object required"))
                        };
                        out.response(id,response);
                    }
                }
                Some(event)=events.recv(), if hello=>d.event(event).await,
                _=tick.tick(), if hello=>d.tick().await,
            }
        } Ok(())
    }.await;
    let shutdown = async {
        let ids: Vec<_> = d.instances.keys().cloned().collect();
        for id in ids {
            d.close_instance(&id, "control_plane_eof").await;
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), shutdown).await;
    out.drain().await;
    result
}

#[cfg(windows)]
fn browser_size(s: Size) -> crate::browser_wire::Size {
    crate::browser_wire::Size {
        width: s.width,
        height: s.height,
        dpr: s.dpr,
    }
}
#[cfg(windows)]
fn browser_target(t: crate::browser_wire::Target) -> Target {
    let mut result = Target::new(
        t.id,
        t.url,
        Size {
            width: t.size.width,
            height: t.size.height,
            dpr: t.size.dpr,
        },
        t.active,
    );
    result.generation = t.generation.to_string();
    result.title = t.title;
    result.opener_target_id = (t.opener != 0).then(|| t.opener.to_string());
    result.kind = if t.kind == 2 { "popup" } else { "tab" };
    result.load = ["idle", "loading", "complete", "failed"][t.load as usize];
    result.complete_at = None;
    result
}
#[cfg(windows)]
impl Daemon {
    async fn browser_command(&mut self, method: &str, p: &Value) -> Result<Value> {
        let i = self.instance(p)?;
        let id = i.id.clone();
        let browser = i
            .browser
            .clone()
            .ok_or_else(|| Error::new("invalid_state", "Browser unavailable"))?;
        let tid = if method == "target.create" {
            0
        } else {
            self.target_id(p)?
        };
        let size = p
            .get("size")
            .map(Size::parse)
            .transpose()?
            .unwrap_or(i.options.size);
        let url = url(p
            .get("url")
            .map_or(Ok("about:blank"), |_| string(p, "url"))?)?;
        let activate = boolean(p, "activate", true)?;
        let method = method.to_owned();
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
            Ok(match method.as_str() {
                "target.create" => json!({"target":browser_target(browser.create(&url,browser_size(size),activate)?)}),
                "target.navigate" => { browser.navigate(tid,&url)?; json!({"accepted":true}) },
                "target.resize" => json!({"target":browser_target(browser.resize(tid,browser_size(size))?)}),
                "target.activate" => json!({"target":browser_target(browser.activate(tid)?)}),
                "target.close" => { browser.close(tid)?; json!({"closed":true}) },
                _ => anyhow::bail!("Unsupported browser command"),
            })
        }).await.map_err(|e| Error::new("internal",e.to_string()))?.map_err(|e| Error::new("internal", e.to_string()))?;
        // Apply ordered browser notifications before the correlated response.
        self.tick().await;
        self.instances[&id].sync(false);
        self.broadcast(&id).await;
        Ok(result)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.startup_cancel.store(true, Ordering::Release);
    }
}
