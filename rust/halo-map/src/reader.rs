//! Bounds-checked little-endian reads over the inflated map image, and the
//! translation of the engine's absolute pointers to offsets in that image.

use crate::error::{malformed, Result};

pub(crate) struct Raw<'a>(pub &'a [u8]);

impl<'a> Raw<'a> {
    pub fn bytes(&self, off: usize, len: usize) -> Result<&'a [u8]> {
        match off.checked_add(len).and_then(|end| self.0.get(off..end)) {
            Some(b) => Ok(b),
            None => malformed(format!("read of {len} bytes at 0x{off:X} is outside the file")),
        }
    }
    pub fn u32(&self, off: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(off, 4)?.try_into().unwrap()))
    }
    pub fn i32(&self, off: usize) -> Result<i32> {
        Ok(self.u32(off)? as i32)
    }
    pub fn i16(&self, off: usize) -> Result<i16> {
        Ok(i16::from_le_bytes(self.bytes(off, 2)?.try_into().unwrap()))
    }
    pub fn u16(&self, off: usize) -> Result<u16> {
        Ok(self.i16(off)? as u16)
    }
    pub fn u8(&self, off: usize) -> Result<u8> {
        Ok(self.bytes(off, 1)?[0])
    }
    pub fn f32(&self, off: usize) -> Result<f32> {
        Ok(f32::from_bits(self.u32(off)?))
    }
    pub fn f32s<const N: usize>(&self, off: usize) -> Result<[f32; N]> {
        let mut out = [0.0; N];
        for (i, v) in out.iter_mut().enumerate() {
            *v = self.f32(off + i * 4)?;
        }
        Ok(out)
    }
    pub fn i16s<const N: usize>(&self, off: usize) -> Result<[i16; N]> {
        let mut out = [0; N];
        for (i, v) in out.iter_mut().enumerate() {
            *v = self.i16(off + i * 2)?;
        }
        Ok(out)
    }
    /// A NUL-terminated string of at most `max` bytes.
    pub fn cstr(&self, off: usize, max: usize) -> Result<String> {
        let avail = self.0.len().saturating_sub(off).min(max);
        let b = self.bytes(off, avail)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }
}

/// A region of the file that the engine loads at a fixed virtual address:
/// pointers inside it are absolute addresses.
#[derive(Clone, Copy)]
pub(crate) struct Space {
    pub what: &'static str,
    pub base: u32,
    pub file_offset: usize,
    pub size: usize,
}

impl Space {
    /// The file offset of absolute pointer `ptr`, checking `len` bytes fit in the region.
    pub fn resolve(&self, ptr: u32, len: usize) -> Result<usize> {
        let rel = ptr.wrapping_sub(self.base) as usize;
        if ptr < self.base || rel.checked_add(len).is_none_or(|e| e > self.size) {
            return malformed(format!(
                "pointer 0x{ptr:08X} (+{len} bytes) is outside {} [0x{:08X}, +0x{:X})",
                self.what, self.base, self.size
            ));
        }
        Ok(self.file_offset + rel)
    }

    /// `struct tag_block { long count; void *address; definition* }` at file
    /// offset `off`: the element count and the file offset of the elements.
    pub fn block(&self, raw: &Raw, off: usize, element_size: usize) -> Result<(usize, usize)> {
        let count = raw.i32(off)?;
        let addr = raw.u32(off + 4)?;
        if count < 0 {
            return malformed(format!("negative block count {count} at 0x{off:X}"));
        }
        if count == 0 {
            return Ok((0, 0));
        }
        let count = count as usize;
        let Some(bytes) = count.checked_mul(element_size) else {
            return malformed("block size overflows");
        };
        Ok((count, self.resolve(addr, bytes)?))
    }
}
