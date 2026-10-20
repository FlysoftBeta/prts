//! Registered shared-memory CPU readback FALLBACK. M4 can add a GPU resource
//! variant here without changing browser targets or the video source contract.
use crate::browser_wire::{Decoder, Record};
use anyhow::{Result, ensure};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::atomic::{Ordering, fence},
    time::Instant,
};
use windows::Win32::{
    Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
    System::{Memory::*, Threading::GetCurrentProcess},
};

pub struct CpuFrame {
    pub target: u64,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub received: Instant,
    pub timestamp_us: u64,
}
struct Section {
    handle: HANDLE,
    address: usize,
    target: u64,
    generation: u64,
    width: u32,
    height: u32,
    stride: usize,
    length: usize,
    last_frame: u64,
}
// Access is serialized by the browser Session mutex; the mapping is read-only.
unsafe impl Send for Section {}
impl Drop for Section {
    fn drop(&mut self) {
        unsafe {
            if self.address != 0 {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.address as *mut _,
                });
            }
            let _ = CloseHandle(self.handle);
        }
    }
}
#[derive(Default)]
pub struct Resources {
    sections: BTreeMap<u64, Section>,
    // Coalesce within a target, never across independent subscriptions. At
    // most two registered sections can contribute a pending CPU copy.
    pending: VecDeque<CpuFrame>,
    pub received: u64,
    pub coalesced: u64,
}
impl Resources {
    pub fn take_frame(&mut self) -> Option<CpuFrame> {
        self.pending.pop_front()
    }
    pub fn resource_count(&self) -> usize {
        self.sections.len()
    }
    pub fn process(&mut self, r: &Record, process: HANDLE) -> Result<Record> {
        let mut d = Decoder::new(&r.payload);
        let reply = match r.kind {
            0x300 => {
                ensure!(
                    r.id != 0 && r.id.is_multiple_of(2),
                    "Invalid registration ID"
                );
                let target = d.u64()?;
                let generation = d.u64()?;
                let id = d.u64()?;
                let kind = d.u8()?;
                let source = d.u64()?;
                let width = d.u32()?;
                let height = d.u32()?;
                let format = d.u8()?;
                let stride = d.u32()? as usize;
                let length = d.u64()?;
                let luid = d.u64()?;
                d.finish()?;
                ensure!(
                    target != 0
                        && generation != 0
                        && id != 0
                        && source != 0
                        && source <= usize::MAX as u64
                        && source != usize::MAX as u64
                        && kind == 2
                        && format == 1
                        && luid == 0,
                    "Invalid CPU section identity/format"
                );
                ensure!(
                    (1..=16384).contains(&width)
                        && (1..=16384).contains(&height)
                        && stride >= width as usize * 4
                        && stride.is_multiple_of(4)
                        && length == stride as u64 * height as u64
                        && length <= 256 * 1024 * 1024,
                    "Invalid section layout"
                );
                ensure!(
                    self.sections.len() < 2 && !self.sections.contains_key(&id),
                    "Section pool exceeds two resources"
                );
                let mut handle = HANDLE::default();
                unsafe {
                    DuplicateHandle(
                        process,
                        HANDLE(source as usize as *mut _),
                        GetCurrentProcess(),
                        &mut handle,
                        0,
                        false,
                        DUPLICATE_SAME_ACCESS,
                    )?;
                }
                let mut section = Section {
                    handle,
                    address: 0,
                    target,
                    generation,
                    width,
                    height,
                    stride,
                    length: length as usize,
                    last_frame: 0,
                };
                let map = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, section.length) };
                ensure!(
                    !map.Value.is_null(),
                    "Cannot map CPU section (type/size/access)"
                );
                section.address = map.Value as usize;
                // A successful map is not proof that its pages can be read:
                // SEC_RESERVE can map uncommitted pages, and SEC_IMAGE ignores
                // the requested view length. Only accept a contiguous committed
                // read-only data view covering the entire declared layout.
                let mut region = MEMORY_BASIC_INFORMATION::default();
                let queried =
                    unsafe { VirtualQuery(Some(map.Value), &mut region, size_of_val(&region)) };
                ensure!(
                    queried == size_of_val(&region)
                        && region.BaseAddress == map.Value
                        && region.AllocationBase == map.Value
                        && region.State == MEM_COMMIT
                        && region.Type == MEM_MAPPED
                        && region.Protect == PAGE_READONLY
                        && region.RegionSize >= section.length,
                    "CPU section is not a committed readable data view of the declared size"
                );
                self.sections.insert(id, section);
                Record {
                    kind: 7,
                    id: r.id,
                    payload: vec![],
                }
            }
            0x301 => {
                ensure!(
                    r.id != 0 && r.id.is_multiple_of(2),
                    "Invalid unregistration ID"
                );
                let id = d.u64()?;
                d.finish()?;
                ensure!(
                    self.sections.remove(&id).is_some(),
                    "Unknown unregistration"
                );
                self.pending.retain(|f| {
                    self.sections
                        .values()
                        .any(|s| (s.target, s.generation) == (f.target, f.generation))
                });
                Record {
                    kind: 7,
                    id: r.id,
                    payload: vec![],
                }
            }
            0x311 => {
                ensure!(r.id == 0, "Frame notification has request ID");
                let target = d.u64()?;
                let generation = d.u64()?;
                let id = d.u64()?;
                let frame = d.u64()?;
                let timestamp_us = d.u64()?;
                let width = d.u32()?;
                let height = d.u32()?;
                let format = d.u8()?;
                let rects = d.u32()?;
                ensure!(rects <= 64, "Too many damage rects");
                for _ in 0..rects {
                    let x = d.u32()?;
                    let y = d.u32()?;
                    let w = d.u32()?;
                    let h = d.u32()?;
                    ensure!(
                        x <= width && y <= height && w <= width - x && h <= height - y,
                        "Invalid damage"
                    );
                }
                d.finish()?;
                let s = self
                    .sections
                    .get_mut(&id)
                    .ok_or_else(|| anyhow::anyhow!("Unregistered frame"))?;
                ensure!(
                    (target, generation, width, height, format)
                        == (s.target, s.generation, s.width, s.height, 1)
                        && frame > s.last_frame,
                    "Frame metadata mismatch/reuse"
                );
                fence(Ordering::Acquire);
                let capture = Instant::now();
                let src = unsafe { std::slice::from_raw_parts(s.address as *const u8, s.length) };
                let row = width as usize * 4;
                let mut pixels = vec![0; row * height as usize];
                for (dst, src) in pixels.chunks_exact_mut(row).zip(src.chunks_exact(s.stride)) {
                    dst.copy_from_slice(&src[..row]);
                }
                s.last_frame = frame;
                self.received += 1;
                let before = self.pending.len();
                self.pending.retain(|f| f.target != target);
                self.coalesced += (before - self.pending.len()) as u64;
                self.pending.push_back(CpuFrame {
                    target,
                    generation,
                    width,
                    height,
                    pixels,
                    received: capture,
                    timestamp_us,
                });
                // All section reads have completed. The encoder owns its CPU copy.
                let mut payload = id.to_le_bytes().to_vec();
                payload.extend(frame.to_le_bytes());
                payload.push(0);
                Record {
                    kind: 0x320,
                    id: 0,
                    payload,
                }
            }
            _ => anyhow::bail!("Unexpected frame resource message"),
        };
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::PCWSTR;

    fn registration(handle: HANDLE, width: u32, height: u32, stride: u32) -> Record {
        let mut p = Vec::new();
        for n in [1u64, 2, 3] {
            p.extend(n.to_le_bytes());
        }
        p.push(2);
        p.extend((handle.0 as u64).to_le_bytes());
        p.extend(width.to_le_bytes());
        p.extend(height.to_le_bytes());
        p.push(1);
        p.extend(stride.to_le_bytes());
        p.extend((stride as u64 * height as u64).to_le_bytes());
        p.extend(0u64.to_le_bytes());
        Record {
            kind: 0x300,
            id: 2,
            payload: p,
        }
    }
    fn descriptor() -> Record {
        let mut p = Vec::new();
        for n in [1u64, 2, 3, 4, 5] {
            p.extend(n.to_le_bytes());
        }
        p.extend(2u32.to_le_bytes());
        p.extend(2u32.to_le_bytes());
        p.push(1);
        p.extend(0u32.to_le_bytes());
        Record {
            kind: 0x311,
            id: 0,
            payload: p,
        }
    }
    #[test]
    fn shared_section_stride_copy_release_and_lifetime() -> Result<()> {
        let source = unsafe {
            CreateFileMappingW(
                HANDLE(-1isize as *mut _),
                None,
                PAGE_READWRITE,
                0,
                24,
                PCWSTR::null(),
            )?
        };
        let view = unsafe { MapViewOfFile(source, FILE_MAP_WRITE, 0, 0, 24) };
        ensure!(!view.Value.is_null(), "Test mapping failed");
        unsafe {
            std::slice::from_raw_parts_mut(view.Value as *mut u8, 24).copy_from_slice(&[
                1, 2, 3, 255, 4, 5, 6, 255, 99, 99, 99, 99, 7, 8, 9, 255, 10, 11, 12, 255, 99, 99,
                99, 99,
            ]);
        }
        let mut pool = Resources::default();
        assert_eq!(
            pool.process(&registration(source, 2, 2, 12), unsafe {
                GetCurrentProcess()
            })?
            .kind,
            7
        );
        unsafe {
            UnmapViewOfFile(view)?;
            CloseHandle(source)?;
        }
        // Imported section survives closure of its source handle and mapping.
        let frame = descriptor();
        // Browser-controlled descriptor fields must match the registered view
        // before any allocation or pixel read, even for extreme dimensions.
        for (offset, bytes) in [
            (0, 99u64.to_le_bytes().to_vec()),
            (8, 99u64.to_le_bytes().to_vec()),
            (40, u32::MAX.to_le_bytes().to_vec()),
            (44, u32::MAX.to_le_bytes().to_vec()),
            (48, vec![2]),
        ] {
            let mut bad = frame.clone();
            bad.payload[offset..offset + bytes.len()].copy_from_slice(&bytes);
            assert!(pool.process(&bad, unsafe { GetCurrentProcess() }).is_err());
            assert!(pool.pending.is_empty());
        }
        let release = pool.process(&frame, unsafe { GetCurrentProcess() })?;
        assert_eq!(
            (release.kind, release.id, release.payload.len()),
            (0x320, 0, 17)
        );
        assert_eq!(
            pool.pending.front().unwrap().pixels,
            [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
        assert!(
            pool.process(&frame, unsafe { GetCurrentProcess() })
                .is_err()
        );
        assert!(
            pool.process(
                &Record {
                    kind: 0x301,
                    id: 4,
                    payload: 3u64.to_le_bytes().to_vec()
                },
                unsafe { GetCurrentProcess() }
            )
            .is_ok()
        );
        assert!(
            pool.process(&frame, unsafe { GetCurrentProcess() })
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn static_subscriptions_do_not_coalesce_each_other() -> Result<()> {
        let source = unsafe {
            CreateFileMappingW(
                HANDLE(-1isize as *mut _),
                None,
                PAGE_READWRITE,
                0,
                16,
                PCWSTR::null(),
            )?
        };
        let mut pool = Resources::default();
        let process = unsafe { GetCurrentProcess() };
        let first = pool.process(&registration(source, 2, 2, 8), process);
        let mut second = registration(source, 2, 2, 8);
        second.id = 4;
        second.payload[0..8].copy_from_slice(&2u64.to_le_bytes());
        second.payload[16..24].copy_from_slice(&4u64.to_le_bytes());
        let second = pool.process(&second, process);
        unsafe { CloseHandle(source)? };
        first?;
        second?;

        // Both static sources arrive in a single poll batch. A second frame
        // from target 1 must replace only target 1's copy, preserving target 2.
        let mut a = descriptor();
        let mut b = descriptor();
        b.payload[0..8].copy_from_slice(&2u64.to_le_bytes());
        b.payload[16..24].copy_from_slice(&4u64.to_le_bytes());
        pool.process(&a, process)?;
        pool.process(&b, process)?;
        a.payload[24..32].copy_from_slice(&5u64.to_le_bytes());
        pool.process(&a, process)?;
        assert_eq!((pool.received, pool.coalesced), (3, 1));
        assert_eq!(pool.take_frame().unwrap().target, 2);
        assert_eq!(pool.take_frame().unwrap().target, 1);
        assert!(pool.take_frame().is_none());

        // Retiring the last section removes an unread CPU copy too, so rapid
        // subscription changes cannot grow the pending target set.
        a.payload[24..32].copy_from_slice(&6u64.to_le_bytes());
        pool.process(&a, process)?;
        pool.process(
            &Record {
                kind: 0x301,
                id: 6,
                payload: 3u64.to_le_bytes().to_vec(),
            },
            process,
        )?;
        assert!(pool.take_frame().is_none());
        Ok(())
    }
    #[test]
    fn reserved_section_is_rejected_before_pixel_access() -> Result<()> {
        let source = unsafe {
            CreateFileMappingW(
                HANDLE(-1isize as *mut _),
                None,
                PAGE_READWRITE | SEC_RESERVE,
                0,
                65536,
                PCWSTR::null(),
            )?
        };
        let mut pool = Resources::default();
        let result = pool.process(&registration(source, 2, 2, 8), unsafe {
            GetCurrentProcess()
        });
        unsafe { CloseHandle(source)? };
        // MapViewOfFile succeeds for SEC_RESERVE, but reading it would fault.
        assert!(result.is_err(), "Uncommitted browser section was accepted");
        assert_eq!(pool.resource_count(), 0);
        Ok(())
    }
    #[test]
    fn invalid_layout_and_unregistered_frames_are_rejected() {
        let mut pool = Resources::default();
        for (w, h, stride) in [(0, 2, 8), (2, 2, 4), (2, 2, 9), (16384, 16384, 65536)] {
            assert!(
                pool.process(
                    &registration(HANDLE(std::ptr::dangling_mut()), w, h, stride),
                    unsafe { GetCurrentProcess() }
                )
                .is_err()
            );
        }
        assert!(
            pool.process(&descriptor(), unsafe { GetCurrentProcess() })
                .is_err()
        );
    }
}
