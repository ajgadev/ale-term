//! Inline images: the iTerm2 protocol (`OSC 1337 ; File=...`, used by
//! `imgcat`) and the kitty graphics protocol (`APC G ...`, used by
//! `kitten icat`, `timg`, `chafa`, `yazi` and others).
//!
//! This module parses the two protocols and decodes image data. It does not
//! touch the terminal grid; `io` decides where an image goes.
//!
//! Supported from kitty's protocol: direct transmission (`t=d`) of PNG, RGB
//! and RGBA data, optionally zlib-compressed and chunked; display at the
//! cursor; placing an already transmitted image; deletion; and the support
//! query. Not supported: file and shared-memory transmission, animation,
//! z-ordering and Unicode placeholders.

use std::collections::{HashMap, VecDeque};

/// A decoded image waiting to be handed to the renderer.
pub struct Image {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// RGBA, 4 bytes per pixel.
    pub rgba: Vec<u8>,
}

/// Largest image side we accept, in pixels.
const MAX_SIDE: u32 = 8192;

/// How to size one dimension of an iTerm2 image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extent {
    Auto,
    Cells(u32),
    Pixels(u32),
    Percent(u32),
}

impl Extent {
    fn parse(value: &str) -> Extent {
        let number = |digits: &str| digits.parse::<u32>().ok().filter(|n| *n > 0);
        if let Some(n) = value.strip_suffix("px").and_then(number) {
            Extent::Pixels(n)
        } else if let Some(n) = value.strip_suffix('%').and_then(number) {
            Extent::Percent(n.min(100))
        } else if let Some(n) = number(value) {
            Extent::Cells(n)
        } else {
            Extent::Auto
        }
    }

    /// Resolves to pixels given the cell size and the terminal's size along
    /// this dimension.
    fn pixels(self, cell: u32, terminal: u32) -> Option<f32> {
        match self {
            Extent::Auto => None,
            Extent::Cells(n) => Some((n * cell) as f32),
            Extent::Pixels(n) => Some(n as f32),
            Extent::Percent(n) => Some(terminal as f32 * n as f32 / 100.0),
        }
    }
}

/// A parsed `OSC 1337 ; File=` sequence.
pub struct ItermFile {
    pub data: Vec<u8>,
    pub width: Extent,
    pub height: Extent,
    pub preserve_aspect: bool,
}

/// Parses an OSC 1337 payload. Returns `None` unless it is an inline file
/// (other OSC 1337 commands, and files meant for download, are ignored).
pub fn parse_iterm(payload: &[u8]) -> Option<ItermFile> {
    let rest = payload.strip_prefix(b"1337;File=")?;
    let colon = rest.iter().position(|b| *b == b':')?;
    let arguments = std::str::from_utf8(&rest[..colon]).ok()?;

    let mut file = ItermFile {
        data: Vec::new(),
        width: Extent::Auto,
        height: Extent::Auto,
        preserve_aspect: true,
    };
    let mut inline = false;
    for argument in arguments.split(';') {
        let Some((key, value)) = argument.split_once('=') else { continue };
        match key {
            "inline" => inline = value == "1",
            "width" => file.width = Extent::parse(value),
            "height" => file.height = Extent::parse(value),
            "preserveAspectRatio" => file.preserve_aspect = value != "0",
            _ => {}
        }
    }
    if !inline {
        return None;
    }
    file.data = base64_decode(&rest[colon + 1..])?;
    Some(file)
}

/// The on-screen size, in pixels, for an iTerm2 image of `natural` size.
pub fn iterm_display_size(
    file: &ItermFile,
    natural: (u32, u32),
    cell: (u32, u32),
    terminal: (u32, u32),
) -> (f32, f32) {
    let (nw, nh) = (natural.0 as f32, natural.1 as f32);
    let width = file.width.pixels(cell.0, terminal.0);
    let height = file.height.pixels(cell.1, terminal.1);
    match (width, height) {
        (None, None) => (nw, nh),
        (Some(w), None) => (w, w * nh / nw),
        (None, Some(h)) => (h * nw / nh, h),
        (Some(w), Some(h)) if file.preserve_aspect => {
            let scale = (w / nw).min(h / nh);
            (nw * scale, nh * scale)
        }
        (Some(w), Some(h)) => (w, h),
    }
}

/// Decodes a PNG, JPEG or GIF file (first frame) to RGBA.
pub fn decode_file(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let reader = ::image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("unsupported image size {width}x{height}"));
    }
    let decoded = ::image::load_from_memory(data).map_err(|e| e.to_string())?;
    Ok((width, height, decoded.into_rgba8().into_raw()))
}

// ---- kitty graphics protocol ------------------------------------------------

/// The `key=value` control data of a kitty graphics command.
#[derive(Default, Debug, Clone)]
pub struct KittyCommand {
    values: HashMap<u8, String>,
}

impl KittyCommand {
    /// Splits an APC payload (`Gkey=value,...;base64data`) into its control
    /// data and payload. Returns `None` for APCs that are not graphics.
    pub fn parse(apc: &[u8]) -> Option<(KittyCommand, &[u8])> {
        let body = apc.strip_prefix(b"G")?;
        let (control, data) = match body.iter().position(|b| *b == b';') {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, &body[body.len()..]),
        };
        let mut command = KittyCommand::default();
        for pair in std::str::from_utf8(control).ok()?.split(',') {
            if let Some((key, value)) = pair.split_once('=')
                && key.len() == 1
            {
                command.values.insert(key.as_bytes()[0], value.to_owned());
            }
        }
        Some((command, data))
    }

    pub fn number(&self, key: u8) -> Option<u32> {
        self.values.get(&key)?.parse().ok()
    }

    /// A single-letter value such as `a=T`, with a default.
    pub fn letter(&self, key: u8, default: u8) -> u8 {
        self.values.get(&key).and_then(|v| v.as_bytes().first().copied()).unwrap_or(default)
    }
}

/// Decodes the pixel data of a kitty transmission to RGBA.
pub fn decode_kitty(command: &KittyCommand, base64: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if command.letter(b't', b'd') != b'd' {
        return Err("ENOTSUP:only direct transmission is supported".into());
    }
    let mut data = base64_decode(base64).ok_or("EINVAL:bad base64 data")?;
    if command.letter(b'o', 0) == b'z' {
        data = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, 4 * (MAX_SIDE * MAX_SIDE) as usize)
            .map_err(|_| "EINVAL:bad compressed data")?;
    }
    let format = command.number(b'f').unwrap_or(32);
    if format == 100 {
        return decode_file(&data).map_err(|e| format!("EINVAL:{e}"));
    }
    let width = command.number(b's').ok_or("EINVAL:missing width")?;
    let height = command.number(b'v').ok_or("EINVAL:missing height")?;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err("EINVAL:unsupported image size".into());
    }
    let pixels = (width * height) as usize;
    match format {
        32 if data.len() >= pixels * 4 => {
            data.truncate(pixels * 4);
            Ok((width, height, data))
        }
        24 if data.len() >= pixels * 3 => {
            let mut rgba = Vec::with_capacity(pixels * 4);
            for rgb in data[..pixels * 3].chunks(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            Ok((width, height, rgba))
        }
        24 | 32 => Err("EINVAL:not enough pixel data".into()),
        _ => Err("EINVAL:unknown format".into()),
    }
}

/// Decodes base64, ignoring whitespace and padding. `None` on other
/// invalid characters.
pub fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in input {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

// ---- Per-session image state ------------------------------------------------

/// Images of one session that the app has not collected yet, plus the
/// bookkeeping the kitty protocol needs.
#[derive(Default)]
pub struct ImageState {
    next_id: u32,
    /// Decoded images waiting for the app to upload them to the GPU.
    pending: VecDeque<Image>,
    pending_bytes: usize,
    /// Ids of images the program deleted; the app frees their textures.
    removed: Vec<u32>,
    /// kitty image number -> our id and pixel size, for later placement.
    pub(crate) kitty: HashMap<u32, (u32, u32, u32)>,
    /// A chunked kitty transmission in progress.
    pub(crate) partial: Option<(KittyCommand, Vec<u8>)>,
}

/// Decoded images kept while waiting for the app (e.g. for a background
/// tab). Beyond this the oldest are dropped.
const PENDING_BUDGET: usize = 96 * 1024 * 1024;

impl ImageState {
    /// Stores a decoded image and returns its id.
    pub(crate) fn add(&mut self, width: u32, height: u32, rgba: Vec<u8>) -> u32 {
        self.next_id += 1;
        self.pending_bytes += rgba.len();
        self.pending.push_back(Image { id: self.next_id, width, height, rgba });
        while self.pending_bytes > PENDING_BUDGET && self.pending.len() > 1 {
            if let Some(dropped) = self.pending.pop_front() {
                self.pending_bytes -= dropped.rgba.len();
            }
        }
        self.next_id
    }

    pub(crate) fn remove(&mut self, id: u32) {
        self.pending.retain(|image| image.id != id);
        self.pending_bytes = self.pending.iter().map(|image| image.rgba.len()).sum();
        self.removed.push(id);
    }

    /// Hands over the images decoded since the last call and the ids of
    /// images deleted since then.
    pub fn take(&mut self) -> (Vec<Image>, Vec<u32>) {
        self.pending_bytes = 0;
        (self.pending.drain(..).collect(), std::mem::take(&mut self.removed))
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.removed.is_empty()
    }
}

/// The hyperlink scheme used to tag the cells an image covers.
pub const IMAGE_TAG: &str = "ale-img";

/// One row of an image as recorded in the grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageCell {
    pub id: u32,
    /// Which row of the image this cell belongs to.
    pub row: u32,
    /// Displayed size of the whole image, in cells (may be fractional).
    pub width_cells: f32,
    pub height_cells: f32,
}

impl ImageCell {
    pub fn encode(&self) -> String {
        let milli = |cells: f32| (cells * 1000.0).round() as u32;
        format!("{IMAGE_TAG}:{}:{}:{}:{}", self.id, self.row, milli(self.width_cells), milli(self.height_cells))
    }

    /// Reads a tag written by [`ImageCell::encode`].
    pub fn decode(uri: &str) -> Option<ImageCell> {
        let mut fields = uri.strip_prefix(IMAGE_TAG)?.strip_prefix(':')?.split(':');
        let mut next = || fields.next()?.parse::<u32>().ok();
        let (id, row, width, height) = (next()?, next()?, next()?, next()?);
        Some(ImageCell { id, row, width_cells: width as f32 / 1000.0, height_cells: height as f32 / 1000.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x1 PNG: one red pixel, one blue pixel.
    fn tiny_png() -> Vec<u8> {
        let mut out = Vec::new();
        let image = ::image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        image.write_to(&mut std::io::Cursor::new(&mut out), ::image::ImageFormat::Png).unwrap();
        out
    }

    fn base64_encode(data: &[u8]) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let n = chunk.iter().fold(0u32, |n, b| (n << 8) | *b as u32) << (8 * (3 - chunk.len()));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn base64_round_trips() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"hello world!", &[0, 255, 128, 7]] {
            assert_eq!(base64_decode(base64_encode(data).as_bytes()).as_deref(), Some(data));
        }
        assert_eq!(base64_decode(b"aGk=\n"), Some(b"hi".to_vec()));
        assert_eq!(base64_decode(b"a$b"), None);
    }

    #[test]
    fn parses_and_decodes_an_iterm_image() {
        let payload = format!("1337;File=name=eC5wbmc=;inline=1;width=10;height=50%:{}", base64_encode(&tiny_png()));
        let file = parse_iterm(payload.as_bytes()).unwrap();
        assert_eq!((file.width, file.height), (Extent::Cells(10), Extent::Percent(50)));
        let (width, height, rgba) = decode_file(&file.data).unwrap();
        assert_eq!((width, height), (2, 1));
        assert_eq!(rgba, [255, 0, 0, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn iterm_files_that_are_not_inline_are_ignored() {
        assert!(parse_iterm(b"1337;File=name=x:AAAA").is_none());
        assert!(parse_iterm(b"1337;SetMark").is_none());
    }

    #[test]
    fn iterm_sizing() {
        let file = |width, height, preserve_aspect| ItermFile { data: vec![], width, height, preserve_aspect };
        let size = |f: &ItermFile| iterm_display_size(f, (200, 100), (10, 20), (800, 600));
        assert_eq!(size(&file(Extent::Auto, Extent::Auto, true)), (200.0, 100.0));
        assert_eq!(size(&file(Extent::Cells(10), Extent::Auto, true)), (100.0, 50.0));
        assert_eq!(size(&file(Extent::Auto, Extent::Pixels(50), true)), (100.0, 50.0));
        assert_eq!(size(&file(Extent::Percent(50), Extent::Percent(50), true)), (400.0, 200.0));
        assert_eq!(size(&file(Extent::Percent(50), Extent::Percent(50), false)), (400.0, 300.0));
    }

    #[test]
    fn parses_kitty_commands() {
        let (command, data) = KittyCommand::parse(b"Ga=T,f=24,s=1,v=2,i=7;AAAA").unwrap();
        assert_eq!(command.letter(b'a', b't'), b'T');
        assert_eq!(command.number(b'i'), Some(7));
        assert_eq!(command.number(b'c'), None);
        assert_eq!(data, b"AAAA");
        let (command, data) = KittyCommand::parse(b"Ga=q").unwrap();
        assert_eq!((command.letter(b'a', b't'), data.len()), (b'q', 0));
        assert!(KittyCommand::parse(b"Xfoo").is_none());
    }

    #[test]
    fn decodes_kitty_pixel_formats() {
        let (rgb, _) = KittyCommand::parse(b"Gf=24,s=2,v=1").unwrap();
        let data = base64_encode(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(decode_kitty(&rgb, data.as_bytes()).unwrap(), (2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]));

        let (png, _) = KittyCommand::parse(b"Gf=100").unwrap();
        assert_eq!(decode_kitty(&png, base64_encode(&tiny_png()).as_bytes()).unwrap().0, 2);

        let (compressed, _) = KittyCommand::parse(b"Gf=32,s=1,v=1,o=z").unwrap();
        let zipped = miniz_oxide::deflate::compress_to_vec_zlib(&[9, 8, 7, 6], 6);
        assert_eq!(decode_kitty(&compressed, base64_encode(&zipped).as_bytes()).unwrap(), (1, 1, vec![9, 8, 7, 6]));

        assert!(decode_kitty(&rgb, b"AAAA").unwrap_err().starts_with("EINVAL"));
        let (file, _) = KittyCommand::parse(b"Gf=100,t=f").unwrap();
        assert!(decode_kitty(&file, b"AAAA").unwrap_err().starts_with("ENOTSUP"));
    }

    #[test]
    fn image_cell_tags_round_trip() {
        let cell = ImageCell { id: 12, row: 3, width_cells: 10.5, height_cells: 4.25 };
        assert_eq!(cell.encode(), "ale-img:12:3:10500:4250");
        assert_eq!(ImageCell::decode(&cell.encode()), Some(cell));
        assert_eq!(ImageCell::decode("ale-prompt:"), None);
        assert_eq!(ImageCell::decode("https://x"), None);
    }

    #[test]
    fn pending_images_are_handed_over_once() {
        let mut state = ImageState::default();
        let a = state.add(1, 1, vec![0; 4]);
        let b = state.add(1, 1, vec![0; 4]);
        state.remove(a);
        let (images, removed) = state.take();
        assert_eq!(images.iter().map(|i| i.id).collect::<Vec<_>>(), [b]);
        assert_eq!(removed, [a]);
        assert!(state.is_empty());
    }
}
