//! Latest complete owned scenes. Private caches never retain browser credits.
use crate::{
    browser_frames::CpuFrame,
    composition::{Placement, blend_cpu},
    d3d,
    d3d_composition::Pipeline,
    window_wire::{Layer, Scene},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
const MAX_BYTES: usize = 512 * 1024 * 1024;
struct State {
    scene: Scene,
    cache: BTreeMap<u64, CpuFrame>,
    dirty: bool,
    changed: Instant,
    missing_reported: bool,
}
#[derive(Default)]
pub struct Scenes {
    windows: BTreeMap<u64, State>,
    pipeline: Option<Pipeline>,
    last_window: u64,
    pub missing_layers: u64,
}
fn bytes(frame: &CpuFrame) -> usize {
    frame.width as usize * frame.height as usize * 4
}
fn compatible(layer: &Layer, frame: &CpuFrame) -> bool {
    (
        layer.source,
        layer.generation,
        layer.width,
        layer.height,
        layer.format,
    ) == (
        frame.target,
        frame.generation,
        frame.width,
        frame.height,
        frame.format,
    )
}
impl Scenes {
    pub fn cached_frame(&self, window: u64, source: u64) -> Option<&CpuFrame> {
        self.windows.get(&window)?.cache.get(&source)
    }
    pub fn update(&mut self, scene: Scene) -> Result<()> {
        // The v2 producer currently admits two browser-window subscriptions.
        ensure!(
            self.windows.contains_key(&scene.window) || self.windows.len() < 256,
            "Scene owner limit"
        );
        let previous = self.windows.remove(&scene.window);
        let mut state = State {
            scene,
            cache: BTreeMap::new(),
            dirty: true,
            changed: Instant::now(),
            missing_reported: false,
        };
        if let Some(previous) = previous {
            ensure!(
                state.scene.revision > previous.scene.revision,
                "Stale scene revision"
            );
            if previous.scene.generation == state.scene.generation {
                state.cache = previous
                    .cache
                    .into_iter()
                    .filter(|(_, f)| state.scene.layers.iter().any(|l| compatible(l, f)))
                    .collect();
            }
        }
        self.windows.insert(state.scene.window, state);
        Ok(())
    }
    pub fn remove(&mut self, window: u64) {
        self.windows.remove(&window);
    }
    pub fn push(&mut self, frame: CpuFrame) -> Result<()> {
        let Some(owner) = self
            .windows
            .values()
            .find(|s| s.scene.layers.iter().any(|l| compatible(l, &frame)))
            .map(|s| s.scene.window)
        else {
            // Late frame from a retired scene: its private copy can be dropped.
            return Ok(());
        };
        let total: usize = self
            .windows
            .values()
            .flat_map(|s| s.cache.values())
            .map(bytes)
            .sum();
        let old = self.windows[&owner]
            .cache
            .get(&frame.target)
            .map_or(0, bytes);
        ensure!(
            total - old + bytes(&frame) <= MAX_BYTES,
            "Scene cache exceeds 512MiB"
        );
        let state = self.windows.get_mut(&owner).unwrap();
        state.cache.insert(frame.target, frame);
        state.dirty = true;
        Ok(())
    }
    pub fn take_frame(&mut self) -> Result<Option<CpuFrame>> {
        self.render(None)
    }
    pub fn take_window(&mut self, window: u64) -> Result<Option<CpuFrame>> {
        self.render(Some(window))
    }
    fn render(&mut self, selected: Option<u64>) -> Result<Option<CpuFrame>> {
        let mut order: Vec<_> = self.windows.keys().copied().collect();
        order.sort_by_key(|id| (*id <= self.last_window, *id));
        for window in order {
            if selected.is_some_and(|id| id != window) {
                continue;
            }
            let state = self.windows.get_mut(&window).unwrap();
            if !state.dirty {
                continue;
            }
            let base_layer = state
                .scene
                .layers
                .iter()
                .find(|l| l.role == 1)
                .context("Missing base layer")?;
            let Some(base) = state.cache.get(&base_layer.source) else {
                continue;
            };
            let mut layers: Vec<_> = state
                .scene
                .layers
                .iter()
                .filter(|l| l.role != 1 && l.visible)
                .collect();
            layers.sort_by_key(|l| l.z);
            let missing = layers.iter().any(|l| !state.cache.contains_key(&l.source));
            if missing && state.changed.elapsed() < Duration::from_millis(100) {
                continue;
            }
            if missing && !state.missing_reported {
                eprintln!(
                    "prtsd: missing popup layer after 100ms window={window} revision={}",
                    state.scene.revision
                );
                state.missing_reported = true;
                self.missing_layers += 1;
            }
            let layers: Vec<_> = layers
                .into_iter()
                .filter_map(|l| state.cache.get(&l.source).map(|f| (l, f)))
                .collect();
            let dpr = base_layer.dpr;
            let placements: Vec<_> = layers
                .iter()
                .map(|(l, _)| Placement {
                    x: (l.rect.x * dpr).round() as i32,
                    y: (l.rect.y * dpr).round() as i32,
                    width: (l.rect.width * dpr).round().max(1.) as u32,
                    height: (l.rect.height * dpr).round().max(1.) as u32,
                    corners: l.corners.map(|r| (r * dpr) as f32),
                    opacity: l.opacity as f32,
                    shadow: if l.shadow == 1 {
                        (12. * dpr) as f32
                    } else {
                        0.
                    },
                })
                .collect();
            let (texture, pixels) = if layers.is_empty() {
                (base.texture.clone(), base.pixels.clone())
            } else if let Some(base_gpu) = &base.texture
                && layers.iter().all(|(_, f)| {
                    f.texture
                        .as_ref()
                        .is_some_and(|g| Arc::ptr_eq(&g.gpu, &base_gpu.gpu))
                })
            {
                let gpu = base_gpu.gpu.lock().unwrap();
                if self.pipeline.is_none() {
                    self.pipeline = Some(Pipeline::new(&gpu)?);
                }
                let draws: Vec<_> = placements
                    .iter()
                    .zip(&layers)
                    .map(|(p, (_, f))| (*p, &f.texture.as_ref().unwrap().texture))
                    .collect();
                let texture =
                    self.pipeline
                        .as_ref()
                        .unwrap()
                        .compose(&gpu, &base_gpu.texture, &draws)?;
                (
                    Some(d3d::Frame {
                        texture,
                        gpu: base_gpu.gpu.clone(),
                    }),
                    Vec::new(),
                )
            } else {
                // Mixed transport recovery is an explicit CPU composition path.
                let read = |frame: &CpuFrame| -> Result<Vec<u8>> {
                    if let Some(gpu) = &frame.texture {
                        gpu.gpu.lock().unwrap().readback(&gpu.texture)
                    } else {
                        Ok(frame.pixels.clone())
                    }
                };
                let base_pixels = read(base)?;
                let images = layers
                    .iter()
                    .map(|(_, f)| read(f))
                    .collect::<Result<Vec<_>>>()?;
                let draws: Vec<_> = placements
                    .iter()
                    .zip(&layers)
                    .zip(&images)
                    .map(|((p, (_, f)), image)| (*p, f.width, f.height, image.as_slice()))
                    .collect();
                (
                    None,
                    blend_cpu(&base_pixels, base.width, base.height, &draws)?,
                )
            };
            let received = state.cache.values().map(|f| f.received).max().unwrap();
            let timestamp_us = state.cache.values().map(|f| f.timestamp_us).max().unwrap();
            state.dirty = false;
            self.last_window = window;
            return Ok(Some(CpuFrame {
                texture,
                pixels,
                target: window,
                generation: state.scene.generation,
                width: base.width,
                height: base.height,
                format: 1,
                received,
                timestamp_us,
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_wire::DipRect;
    fn layer(source: u64, base: bool) -> Layer {
        Layer {
            source,
            parent: if base { 0 } else { 10 },
            generation: 1,
            role: if base { 1 } else { 2 },
            rect: DipRect {
                x: 0.,
                y: 0.,
                width: 2.,
                height: 2.,
            },
            input_rect: DipRect {
                x: 0.,
                y: 0.,
                width: 2.,
                height: 2.,
            },
            width: 2,
            height: 2,
            dpr: 1.,
            z: if base { 0 } else { 1 },
            visible: true,
            accepts_input: true,
            format: if base { 1 } else { 2 },
            opacity: 1.,
            corners: [0.; 4],
            shadow: 0,
        }
    }
    fn frame(source: u64, format: u8, color: [u8; 4]) -> CpuFrame {
        CpuFrame {
            texture: None,
            target: source,
            generation: 1,
            width: 2,
            height: 2,
            format,
            pixels: color.repeat(4),
            received: Instant::now(),
            timestamp_us: source,
        }
    }
    #[test]
    fn popup_only_updates_close_restoration_and_stale_epochs() -> Result<()> {
        let mut scenes = Scenes::default();
        let base_scene = Scene {
            window: 1,
            generation: 1,
            revision: 1,
            layers: vec![layer(10, true)],
        };
        scenes.update(base_scene.clone())?;
        scenes.push(frame(10, 1, [20, 40, 60, 255]))?;
        let base = scenes.take_frame()?.unwrap().pixels;
        let mut popup_scene = base_scene.clone();
        popup_scene.revision = 2;
        popup_scene.layers.push(layer(20, false));
        scenes.update(popup_scene)?;
        assert!(
            scenes.take_frame()?.is_none(),
            "Incomplete popup scene emitted too early"
        );
        scenes.push(frame(20, 2, [0, 0, 128, 128]))?;
        let popup = scenes.take_frame()?.unwrap();
        assert_eq!(&popup.pixels[..4], &[10, 20, 158, 255]);
        assert!(
            scenes.take_frame()?.is_none(),
            "Unchanged scene emitted twice"
        );
        let mut closed = base_scene.clone();
        closed.revision = 3;
        scenes.update(closed)?;
        assert_eq!(
            scenes.take_frame()?.unwrap().pixels,
            base,
            "Closed popup left ghost pixels"
        );
        scenes.push(frame(20, 2, [0, 255, 0, 255]))?;
        assert!(scenes.take_frame()?.is_none(), "Retired popup reappeared");
        let mut resized = base_scene;
        resized.generation = 2;
        resized.revision = 4;
        resized.layers[0].generation = 2;
        scenes.update(resized)?;
        scenes.push(frame(10, 1, [255, 0, 0, 255]))?;
        assert!(scenes.take_frame()?.is_none(), "Old resize epoch appeared");
        let mut fresh = frame(10, 1, [0, 255, 0, 255]);
        fresh.generation = 2;
        scenes.push(fresh)?;
        assert_eq!(scenes.take_frame()?.unwrap().generation, 2);
        scenes.remove(1);
        assert!(scenes.windows.is_empty());
        Ok(())
    }
}
