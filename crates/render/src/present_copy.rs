//! Showing CPU-painted frames by copying them to the window, for systems
//! where we have no faster path. Used everywhere except macOS.

use std::num::NonZeroU32;

use crate::WindowRef;
use crate::soft::Frame;

pub struct Presenter {
    surface: softbuffer::Surface<WindowRef, WindowRef>,
    pixels: Vec<u32>,
    painted: Vec<u64>,
    width: u32,
    /// Something has been shown at the current size.
    shown: bool,
}

impl Presenter {
    pub fn new(window: WindowRef) -> Result<Presenter, String> {
        let context = softbuffer::Context::new(window.clone()).map_err(|e| e.to_string())?;
        let surface = softbuffer::Surface::new(&context, window).map_err(|e| e.to_string())?;
        Ok(Presenter { surface, pixels: Vec::new(), painted: Vec::new(), width: 0, shown: false })
    }

    pub fn resize(&mut self, width: u32, height: u32, bands: usize) {
        self.width = width;
        self.pixels = vec![0; (width * height) as usize];
        self.painted = vec![0; bands];
        self.shown = false;
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height))
            && let Err(e) = self.surface.resize(w, h)
        {
            log::warn!("cannot resize the window surface: {e}");
        }
    }

    pub fn shown(&self) -> Option<&[u64]> {
        self.shown.then_some(self.painted.as_slice())
    }

    /// Copies out the picture currently on screen, with its row length in
    /// pixels.
    pub fn read_shown(&self) -> Option<(Vec<u32>, usize)> {
        self.shown.then(|| (self.pixels.clone(), self.width as usize))
    }

    pub fn begin(&mut self) -> Option<Frame<'_>> {
        Some(Frame { pixels: &mut self.pixels, stride: self.width as usize, painted: &mut self.painted })
    }

    pub fn finish(&mut self) {
        match self.surface.buffer_mut() {
            Ok(mut buffer) if buffer.len() == self.pixels.len() => {
                buffer.copy_from_slice(&self.pixels);
                if let Err(e) = buffer.present() {
                    log::warn!("cannot show the frame: {e}");
                }
                self.shown = true;
            }
            Ok(_) => {}
            Err(e) => log::warn!("cannot get a buffer to show the frame in: {e}"),
        }
    }

    /// There is one picture here, and it is the one on screen.
    pub fn trim(&mut self) {}

    /// The picture is kept: a window system may show it again without
    /// asking us to repaint.
    pub fn release(&mut self) {}
}
