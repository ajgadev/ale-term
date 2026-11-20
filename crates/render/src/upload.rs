//! Getting each frame's instance data to the GPU without a copy command.
//!
//! The obvious way to upload (`Queue::write_buffer`) copies the data into a
//! temporary buffer and then has the GPU copy that into the real one. On
//! Apple GPUs any copy command makes the driver set aside about 60 MB of
//! working memory, kept for several seconds after the last copy; doing one
//! every frame holds that memory for as long as the screen is animating.
//!
//! Instead we keep a few buffers that the CPU can write into directly.
//! A buffer cannot be written while the GPU may still be reading it, so
//! they are used in rotation: write one, draw with it, and ask for it to be
//! made writable again, which completes once the GPU is done with it. By
//! the time its turn comes around it is ready.
//!
//! This needs a GPU feature (buffers that are both writable from the CPU
//! and usable for drawing). Where it is missing we fall back to copying.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Enough that one is always writable while the others are being drawn.
const RING_SIZE: usize = 3;

struct Slot {
    buffer: wgpu::Buffer,
    /// Bytes it can hold.
    capacity: u64,
    /// True while the buffer is mapped, i.e. writable by the CPU.
    writable: Arc<AtomicBool>,
}

pub struct InstanceUploader {
    /// Rotating directly-writable buffers, or empty when unsupported.
    ring: Vec<Slot>,
    next: usize,
    /// Used when direct writing is not available.
    fallback: Option<(wgpu::Buffer, u64)>,
    direct: bool,
}

fn slot(device: &wgpu::Device, capacity: u64) -> Slot {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances (direct)"),
        size: capacity,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::MAP_WRITE,
        // Starts out writable.
        mapped_at_creation: true,
    });
    Slot { buffer, capacity, writable: Arc::new(AtomicBool::new(true)) }
}

/// Rounds a size up so buffers are replaced rarely as content grows.
fn grown(bytes: u64) -> u64 {
    bytes.next_power_of_two().max(256 * 1024)
}

impl InstanceUploader {
    /// The GPU feature direct writing needs; request it when available.
    pub const FEATURE: wgpu::Features = wgpu::Features::MAPPABLE_PRIMARY_BUFFERS;

    pub fn new(device: &wgpu::Device) -> Self {
        let direct = device.features().contains(Self::FEATURE);
        Self { ring: Vec::new(), next: 0, fallback: None, direct }
    }

    /// Puts `bytes` in a buffer the GPU can draw from and returns it.
    ///
    /// Call [`InstanceUploader::submitted`] after submitting the commands
    /// that use the returned buffer.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) -> wgpu::Buffer {
        let needed = bytes.len().max(4) as u64;
        if !self.direct {
            if self.fallback.as_ref().is_none_or(|(_, capacity)| *capacity < needed) {
                let capacity = grown(needed);
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("instances"),
                    size: capacity,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.fallback = Some((buffer, capacity));
            }
            let (buffer, _) = self.fallback.as_ref().expect("just created");
            queue.write_buffer(buffer, 0, bytes);
            return buffer.clone();
        }

        if self.ring.is_empty() {
            self.ring = (0..RING_SIZE).map(|_| slot(device, grown(needed))).collect();
        }
        // Let earlier "make writable again" requests complete.
        let _ = device.poll(wgpu::PollType::Poll);
        let mut index = (0..RING_SIZE)
            .map(|offset| (self.next + offset) % RING_SIZE)
            .find(|i| self.ring[*i].writable.load(Ordering::Acquire));
        if index.is_none() {
            // Every buffer is still with the GPU (we are drawing faster
            // than it finishes): wait for it rather than allocate more.
            let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
            index = (0..RING_SIZE).find(|i| self.ring[*i].writable.load(Ordering::Acquire));
        }
        // Still none (should not happen): replace one outright.
        let index = index.unwrap_or(self.next);
        let entry = &mut self.ring[index];
        if entry.capacity < needed || !entry.writable.load(Ordering::Acquire) {
            *entry = slot(device, grown(needed).max(entry.capacity));
        }
        if !bytes.is_empty()
            && let Ok(mut view) = entry.buffer.slice(..bytes.len() as u64).get_mapped_range_mut()
        {
            view.copy_from_slice(bytes);
        }
        entry.buffer.unmap();
        entry.writable.store(false, Ordering::Release);
        self.next = (index + 1) % RING_SIZE;
        entry.buffer.clone()
    }

    /// Call after the commands drawing from the last uploaded buffer have
    /// been submitted: asks for that buffer to become writable again.
    pub fn submitted(&mut self) {
        if !self.direct || self.ring.is_empty() {
            return;
        }
        let index = (self.next + RING_SIZE - 1) % RING_SIZE;
        let entry = &self.ring[index];
        let writable = entry.writable.clone();
        entry.buffer.map_async(wgpu::MapMode::Write, .., move |result| {
            if result.is_ok() {
                writable.store(true, Ordering::Release);
            }
        });
    }
}
