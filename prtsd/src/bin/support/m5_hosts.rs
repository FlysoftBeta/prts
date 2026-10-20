#[derive(Debug)]
struct HostProbe {
    source: u64,
    generation: u64,
    owner: u64,
    kind: u32,
    bounds: [i32; 4],
    input: bool,
    translucent: bool,
    root_bounds: [i32; 4],
    host_pixels: [i32; 4],
    owner_pixels: [i32; 4],
}
impl Client {
    fn hosts(&mut self, id: u64) -> Result<Vec<HostProbe>> {
        let r = self.request(0x42c, id.to_le_bytes().to_vec())?;
        ensure!(r.kind == 0x48c, "Host probe failed: {r:?}");
        let mut d = v1::Decoder::new(&r.payload);
        let count = d.u32()?;
        ensure!(count <= 256, "Unbounded host registry");
        let mut hosts = Vec::new();
        for _ in 0..count {
            hosts.push(HostProbe {
                source: d.u64()?,
                generation: d.u64()?,
                owner: d.u64()?,
                kind: d.u32()?,
                bounds: [
                    d.u32()? as i32,
                    d.u32()? as i32,
                    d.u32()? as i32,
                    d.u32()? as i32,
                ],
                input: d.boolean()?,
                translucent: d.boolean()?,
                root_bounds: [
                    d.u32()? as i32,
                    d.u32()? as i32,
                    d.u32()? as i32,
                    d.u32()? as i32,
                ],
                host_pixels: [
                    d.u32()? as i32,
                    d.u32()? as i32,
                    d.u32()? as i32,
                    d.u32()? as i32,
                ],
                owner_pixels: [
                    d.u32()? as i32,
                    d.u32()? as i32,
                    d.u32()? as i32,
                    d.u32()? as i32,
                ],
            });
        }
        d.finish()?;
        Ok(hosts)
    }
    fn wait_hosts(&mut self, id: u64, visible: bool) -> Result<Vec<HostProbe>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let hosts = self.hosts(id)?;
            if hosts.is_empty() != visible {
                return Ok(hosts);
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Owned host visibility timeout: expected visible={visible}, hosts={hosts:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
fn host_gate(c: &mut Client, id: u64) -> Result<()> {
    let other = c.create("about:blank")?;
    ensure!(
        c.request(WINDOW_ACTIVATE, id.to_le_bytes().to_vec())?.kind == WINDOW_RESULT,
        "activate failed"
    );
    let w = c
        .list()?
        .into_iter()
        .find(|w| w.id == id)
        .context("Host gate window missing")?;
    c.send(&w, v1::Input::Focus(true), 0)?;
    c.wait_hosts(id, false)?;
    for cycle in 0..3 {
        c.key(&w, 18, "AltLeft", "Alt", 4, true)?;
        c.key(&w, 70, "KeyF", "f", 4, true)?;
        c.key(&w, 70, "KeyF", "f", 4, false)?;
        c.key(&w, 18, "AltLeft", "Alt", 0, false)?;
        let hosts = c.wait_hosts(id, true)?;
        let mut sources = std::collections::BTreeSet::new();
        for host in &hosts {
            ensure!(
                host.owner == id && sources.insert(host.source) && host.generation > 0,
                "Invalid host identity/owner: {host:?}"
            );
            ensure!(
                host.bounds[2] > 0 && host.bounds[3] > 0,
                "Unready host geometry: {host:?}"
            );
            // Read all diagnostic properties so output can diagnose wrong
            // native-widget type, event policy or transparency classification.
            println!(
                "HOST kind={} input={} translucent={} root={:?} host_px={:?} owner_px={:?} {host:?}",
                host.kind,
                host.input,
                host.translucent,
                host.root_bounds,
                host.host_pixels,
                host.owner_pixels
            );
            ensure!(
                host.host_pixels[0] >= host.owner_pixels[0]
                    && host.host_pixels[1] >= host.owner_pixels[1]
                    && host.host_pixels[0] + host.host_pixels[2]
                        <= host.owner_pixels[0] + host.owner_pixels[2]
                    && host.host_pixels[1] + host.host_pixels[3]
                        <= host.owner_pixels[1] + host.owner_pixels[3],
                "Owned menu lies outside the streamed owner canvas: {host:?}"
            );
        }
        ensure!(
            c.hosts(other.id)?.is_empty(),
            "A's menu leaked into B's owned host family"
        );
        c.press(&w, 27, "Escape", "Escape")?;
        c.wait_hosts(id, false)?;
        println!("PASS owned real app-menu open/close cycle {cycle}, cross-owner exclusion");
    }
    ensure!(
        c.request(WINDOW_CLOSE, other.id.to_le_bytes().to_vec())?
            .kind
            == v1::OK,
        "close failed"
    );
    println!("PASS M5b initial host ownership/lifecycle gate (composition still pending)");
    Ok(())
}
