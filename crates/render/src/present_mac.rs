//! Showing CPU-painted frames on macOS without copying them.
//!
//! The picture lives in an `IOSurface`: a block of memory that this process
//! and the system's window compositor both have mapped. We paint straight
//! into it and tell our layer "show this surface"; nothing is copied and no
//! GPU commands are issued from this process.
//!
//! Two surfaces are used in turn (double buffering), so the compositor is
//! never shown a surface that is being painted. Each remembers which band
//! contents it holds, since a surface that sat out a frame is one frame
//! behind.
//!
//! A surface is as large as the window's picture (tens of megabytes on a
//! large screen), so they are not kept longer than needed: the one that is
//! not on screen can be let go while nothing changes, and both while the
//! window cannot be seen. They are made again when next painted.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, msg_send};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_io_surface::{
    IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat,
    kIOSurfaceWidth,
};
use objc2_quartz_core::{CALayer, CATransaction};
use raw_window_handle::RawWindowHandle;

use crate::WindowRef;
use crate::soft::Frame;

struct Buffer {
    surface: CFRetained<IOSurfaceRef>,
    /// Pixels per row, which may exceed the width (rows are aligned).
    stride: usize,
    /// Hash of what each band of this surface currently shows.
    painted: Vec<u64>,
}

pub struct Presenter {
    /// Our own layer inside the window's view.
    layer: Retained<CALayer>,
    /// The view's layer; we copy its size and scale.
    root: Retained<CALayer>,
    /// The two surfaces; either may be missing until it is next painted.
    buffers: [Option<Buffer>; 2],
    /// Which buffer is on screen.
    shown: Option<usize>,
    /// Which buffer is locked for painting.
    painting: Option<usize>,
    width: u32,
    height: u32,
    bands: usize,
    _window: WindowRef,
}

/// Sets layer properties without the system animating the change.
fn without_animation(change: impl FnOnce()) {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    change();
    CATransaction::commit();
}

fn create_surface(width: u32, height: u32) -> Option<CFRetained<IOSurfaceRef>> {
    // Core Foundation strings and dictionaries are interchangeable with
    // their Foundation counterparts ("toll-free bridging"), which gives us
    // a convenient way to build the property list.
    let key = |name: &'static CFString| -> &NSString {
        // SAFETY: CFString and NSString are toll-free bridged.
        unsafe { &*(name as *const CFString).cast::<NSString>() }
    };
    // SAFETY: these are constants exported by the IOSurface framework.
    let keys = unsafe {
        [key(kIOSurfaceWidth), key(kIOSurfaceHeight), key(kIOSurfaceBytesPerElement), key(kIOSurfacePixelFormat)]
    };
    // 'BGRA': one little-endian 0xAARRGGBB value per pixel.
    let format = u32::from_be_bytes(*b"BGRA");
    let values = [
        NSNumber::new_u32(width),
        NSNumber::new_u32(height),
        NSNumber::new_u32(4),
        NSNumber::new_u32(format),
    ];
    let values: Vec<&NSNumber> = values.iter().map(|value| &**value).collect();
    let properties = NSDictionary::from_slices(&keys, &values);
    // SAFETY: NSDictionary and CFDictionary are toll-free bridged, and the
    // dictionary holds the property types IOSurfaceCreate expects.
    unsafe {
        let properties = &*(Retained::as_ptr(&properties)).cast::<CFDictionary>();
        IOSurfaceRef::new(properties)
    }
}

impl Presenter {
    pub fn new(window: WindowRef) -> Result<Presenter, String> {
        MainThreadMarker::new().ok_or("the window can only be set up from the main thread")?;
        let handle = window.window_handle().map_err(|e| e.to_string())?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err("not a macOS window".into());
        };
        // SAFETY: the handle guarantees a valid `NSView` pointer.
        let view: &NSObject = unsafe { handle.ns_view.cast().as_ref() };
        // SAFETY: `setWantsLayer:` and `layer` are plain `NSView` methods
        // with these signatures.
        let root: Option<Retained<CALayer>> = unsafe {
            let _: () = msg_send![view, setWantsLayer: true];
            msg_send![view, layer]
        };
        let root = root.ok_or("the window's view has no layer")?;

        // A layer of our own, so we never fight the view over its layer.
        let layer = CALayer::new();
        without_animation(|| {
            layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
            // Top-left origin, like our pixel coordinates.
            layer.setGeometryFlipped(true);
            layer.setOpaque(true);
            root.addSublayer(&layer);
        });
        Ok(Presenter {
            layer,
            root,
            buffers: [None, None],
            shown: None,
            painting: None,
            width: 0,
            height: 0,
            bands: 0,
            _window: window,
        })
    }

    /// Sets the size of the picture in pixels; `bands` is how many repaint
    /// bands that makes.
    pub fn resize(&mut self, width: u32, height: u32, bands: usize) {
        self.width = width;
        self.height = height;
        self.bands = bands;
        // New surfaces are created lazily, and start with nothing painted.
        self.buffers = [None, None];
        self.shown = None;
        self.painting = None;
        let scale = self.root.contentsScale().max(1.0);
        let size = CGSize::new(width as f64 / scale, height as f64 / scale);
        without_animation(|| {
            self.layer.setContentsScale(scale);
            self.layer.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), size));
        });
    }

    /// What each band of the picture on screen shows, or `None` if nothing
    /// has been shown at this size yet.
    pub fn shown(&self) -> Option<&[u64]> {
        self.buffers[self.shown?].as_ref().map(|buffer| buffer.painted.as_slice())
    }

    /// Copies out the picture currently on screen, with its row length in
    /// pixels. For checking that what is shown matches what was asked for.
    pub fn read_shown(&self) -> Option<(Vec<u32>, usize)> {
        let buffer = self.buffers[self.shown?].as_ref()?;
        let read_only = IOSurfaceLockOptions::ReadOnly;
        // SAFETY: a read-only lock keeps the memory mapped and stable for
        // the duration of the copy; the length matches the surface.
        unsafe {
            if buffer.surface.lock(read_only, std::ptr::null_mut()) != 0 {
                return None;
            }
            let base = buffer.surface.base_address().as_ptr().cast::<u32>();
            let pixels = std::slice::from_raw_parts(base, buffer.stride * self.height as usize).to_vec();
            buffer.surface.unlock(read_only, std::ptr::null_mut());
            Some((pixels, buffer.stride))
        }
    }

    /// Hands out the surface to paint the next frame into.
    pub fn begin(&mut self) -> Option<Frame<'_>> {
        // The one that is not on screen.
        let index = match self.shown {
            Some(0) => 1,
            _ => 0,
        };
        if self.buffers[index].is_none() {
            let surface = create_surface(self.width, self.height)?;
            let stride = surface.bytes_per_row() / 4;
            self.buffers[index] = Some(Buffer { surface, stride, painted: vec![0; self.bands] });
        }
        let buffer = self.buffers[index].as_mut()?;
        // SAFETY: locking gives this process exclusive access to the
        // surface's memory until the matching unlock in `finish`.
        let status = unsafe { buffer.surface.lock(IOSurfaceLockOptions(0), std::ptr::null_mut()) };
        if status != 0 {
            return None;
        }
        self.painting = Some(index);
        let base: NonNull<c_void> = buffer.surface.base_address();
        // SAFETY: while locked, the surface's memory is `bytes_per_row *
        // height` bytes at `base`, 4-byte aligned, and nobody else writes
        // to it. The slice cannot outlive the borrow of `self`, and
        // `finish` (which unlocks) needs `&mut self`.
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(base.as_ptr().cast::<u32>(), buffer.stride * self.height as usize)
        };
        Some(Frame { pixels, stride: buffer.stride, painted: &mut buffer.painted })
    }

    /// Puts the surface handed out by `begin` on screen.
    pub fn finish(&mut self) {
        let Some(index) = self.painting.take() else { return };
        let Some(buffer) = &self.buffers[index] else { return };
        let surface = &buffer.surface;
        // SAFETY: matches the lock in `begin`.
        unsafe { surface.unlock(IOSurfaceLockOptions(0), std::ptr::null_mut()) };
        without_animation(|| {
            // SAFETY: an IOSurface is a valid value for a layer's contents.
            unsafe {
                let object = &*(CFRetained::as_ptr(surface).as_ptr()).cast::<AnyObject>();
                self.layer.setContents(Some(object));
            }
        });
        self.shown = Some(index);
    }

    /// Lets go of the surface that is not on screen. The next frame makes
    /// a new one and paints all of it.
    pub fn trim(&mut self) {
        for (index, buffer) in self.buffers.iter_mut().enumerate() {
            if Some(index) != self.shown && Some(index) != self.painting {
                *buffer = None;
            }
        }
    }

    /// Lets go of both surfaces, for a window that cannot be seen. The
    /// layer shows nothing until the next frame.
    pub fn release(&mut self) {
        if self.painting.is_some() {
            return;
        }
        // The layer holds on to the surface it shows.
        // SAFETY: a layer may have no contents.
        without_animation(|| unsafe { self.layer.setContents(None) });
        self.buffers = [None, None];
        self.shown = None;
    }
}
