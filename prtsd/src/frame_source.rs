use crate::model::Target;
use std::time::Instant;

pub struct Captured {
    pub pixels: Vec<u8>,
    pub capture: Instant,
}
/// M3 CPU fallback ingress. A GPU surface variant can extend Captured in M4;
/// subscription/refresh and encoder publication remain independent of capture.
pub trait FrameSource: Send {
    fn capture(
        &mut self,
        target: &Target,
        fps: u32,
        refresh: bool,
    ) -> anyhow::Result<Option<Captured>>;
    fn idle(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
pub struct Synthetic {
    pub origin: Instant,
    pub frame: u64,
}
impl FrameSource for Synthetic {
    fn capture(&mut self, t: &Target, _: u32, _: bool) -> anyhow::Result<Option<Captured>> {
        self.frame += 1;
        Ok(Some(Captured {
            capture: Instant::now(),
            pixels: crate::video::render(t, self.frame, self.origin.elapsed().as_secs_f64()),
        }))
    }
}
#[cfg(windows)]
pub type BrowserSlot =
    std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<crate::browser::Browser>>>>;
#[cfg(windows)]
pub struct BrowserSource {
    pub browser: BrowserSlot,
    pub subscribed: Option<(u64, u32)>,
}
#[cfg(windows)]
fn configure_if_present(
    browser: &crate::browser::Browser,
    id: u64,
    fps: u32,
) -> anyhow::Result<bool> {
    match browser.configure_frames(id, fps) {
        Ok(_) => Ok(true),
        // A capture sample can race target closure. Only absence of that
        // exact target is recoverable; transport/resource errors stay fatal.
        Err(e) => {
            if browser.list()?.iter().any(|t| t.id == id) {
                Err(e)
            } else {
                Ok(false)
            }
        }
    }
}
#[cfg(windows)]
impl FrameSource for BrowserSource {
    fn capture(&mut self, t: &Target, fps: u32, refresh: bool) -> anyhow::Result<Option<Captured>> {
        let Some(browser) = self.browser.lock().unwrap().clone() else {
            return Ok(None);
        };
        let id = t.target_id.parse()?;
        if self.subscribed != Some((id, fps)) {
            if let Some((old, _)) = self.subscribed.take() {
                // Closing a subscribed target has already removed its producer.
                configure_if_present(&browser, old, 0)?;
            }
            if !configure_if_present(&browser, id, fps)? {
                return Ok(None);
            }
            self.subscribed = Some((id, fps));
        }
        if refresh && let Err(e) = browser.refresh_frame(id) {
            if browser.list()?.iter().any(|t| t.id == id) {
                return Err(e);
            }
            return Ok(None);
        }
        let Some(frame) = browser.poll_frame()? else {
            return Ok(None);
        };
        if frame.target != id
            || frame.generation.to_string() != t.generation
            || (frame.width, frame.height) != t.size.device()
        {
            return Ok(None);
        }
        Ok(Some(Captured {
            pixels: frame.pixels,
            capture: frame.received,
        }))
    }
    fn idle(&mut self) -> anyhow::Result<()> {
        let Some(browser) = self.browser.lock().unwrap().clone() else {
            return Ok(());
        };
        if let Some((old, _)) = self.subscribed.take() {
            configure_if_present(&browser, old, 0)?;
        }
        browser.poll_frame()?;
        Ok(())
    }
}
