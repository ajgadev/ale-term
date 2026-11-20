//! Packs glyph bitmaps into one texture.
//!
//! Uses "shelf" packing: glyphs are placed left to right in rows, and a new
//! row starts when the current one is full. Glyphs in a terminal are all
//! about the same height, so this wastes very little space.

/// Where a bitmap ended up in the atlas, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub x: u32,
    pub y: u32,
}

pub struct ShelfPacker {
    size: u32,
    /// Top of the current row.
    shelf_y: u32,
    /// Height of the tallest glyph in the current row.
    shelf_height: u32,
    /// Next free x position in the current row.
    cursor_x: u32,
}

impl ShelfPacker {
    /// One pixel of padding around every glyph stops neighbours bleeding
    /// into each other when sampled.
    const PADDING: u32 = 1;

    pub fn new(size: u32) -> Self {
        Self { size, shelf_y: Self::PADDING, shelf_height: 0, cursor_x: Self::PADDING }
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    /// Reserves room for a `width` x `height` bitmap, or returns `None` when
    /// the atlas is full.
    pub fn allocate(&mut self, width: u32, height: u32) -> Option<Slot> {
        let pad = Self::PADDING;
        if width + 2 * pad > self.size {
            return None;
        }
        if self.cursor_x + width + pad > self.size {
            // Start a new row.
            self.shelf_y += self.shelf_height + pad;
            self.shelf_height = 0;
            self.cursor_x = pad;
        }
        if self.shelf_y + height + pad > self.size {
            return None;
        }
        let slot = Slot { x: self.cursor_x, y: self.shelf_y };
        self.cursor_x += width + pad;
        self.shelf_height = self.shelf_height.max(height);
        Some(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_do_not_overlap() {
        let mut packer = ShelfPacker::new(64);
        let mut taken: Vec<(u32, u32, u32, u32)> = Vec::new();
        while let Some(slot) = packer.allocate(10, 12) {
            for &(x, y, w, h) in &taken {
                let apart = slot.x >= x + w || x >= slot.x + 10 || slot.y >= y + h || y >= slot.y + 12;
                assert!(apart, "{slot:?} overlaps ({x},{y})");
            }
            assert!(slot.x + 10 <= 64 && slot.y + 12 <= 64);
            taken.push((slot.x, slot.y, 10, 12));
        }
        // 5 per row (1px padding), 4 rows.
        assert_eq!(taken.len(), 20);
    }

    #[test]
    fn rejects_bitmaps_larger_than_the_atlas() {
        assert_eq!(ShelfPacker::new(16).allocate(32, 4), None);
    }
}
