//! The interpreter's memory: byte-addressed regions holding every value in
//! its C layout, so pointers, fields, slices and containers behave exactly as
//! they do in the generated program.
//!
//! An address keeps its region in the bits above [`SHIFT`] and the offset
//! below. Region bases are aligned to 2^40, so an address is as aligned as its
//! offset, and address arithmetic (`align_forward`) works on them unchanged.

use std::collections::{BTreeMap, HashMap};

/// A compile-time address. Zero is `nil`.
pub(crate) type Addr = u64;

/// Bits of an address that hold the offset within its region.
pub(crate) const SHIFT: u32 = 40;
const OFFSET_MASK: u64 = (1 << SHIFT) - 1;

/// Region tag of addresses that name a Wid function (`method(:f)`).
pub(crate) const FN_TAG: u64 = 0xF;
/// Region tag of addresses that name a procedure the interpreter implements
/// itself, like the compile-time heap allocator.
pub(crate) const NATIVE_TAG: u64 = 0xE;

/// Where a value lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Region {
    /// String literals, globals and other data that lives for the whole run.
    Static = 1,
    /// Locals and temporaries of the running calls.
    Stack = 2,
    /// Memory handed out by allocators.
    Heap = 3,
}

/// Why a memory access failed.
#[derive(Debug, Clone)]
pub(crate) enum MemError {
    /// The address is `nil`.
    Nil,
    /// The address is outside every live object.
    Invalid(Addr),
    /// `free` of something that is not a live heap block.
    BadFree(Addr),
    /// The program used more memory than compile time allows.
    Limit(u64),
}

/// The regions and the bookkeeping of heap blocks.
pub(crate) struct Memory {
    regions: [Vec<u8>; 3],
    /// Live heap blocks: offset to size.
    blocks: BTreeMap<u64, u64>,
    /// Interned NUL-terminated string data, by content.
    strings: HashMap<Vec<u8>, Addr>,
    limit: u64,
}

impl Memory {
    /// Creates empty memory that may grow to `limit` bytes in total.
    pub fn new(limit: u64) -> Self {
        // A leading byte in each region keeps offset 0 (and so address 0 in
        // no region) from ever naming an object.
        Memory { regions: [vec![0], vec![0], vec![0]], blocks: BTreeMap::new(), strings: HashMap::new(), limit }
    }

    fn total(&self) -> u64 {
        self.regions.iter().map(|r| r.len() as u64).sum()
    }

    /// Reserves `size` zeroed bytes aligned to `align` in a region.
    pub fn alloc(&mut self, region: Region, size: u64, align: u64) -> Result<Addr, MemError> {
        let align = align.max(1);
        if self.total().saturating_add(size).saturating_add(align) > self.limit {
            return Err(MemError::Limit(self.limit));
        }
        let r = &mut self.regions[region as usize - 1];
        let start = (r.len() as u64).div_ceil(align) * align;
        r.resize((start + size) as usize, 0);
        if region == Region::Heap {
            self.blocks.insert(start, size);
        }
        Ok(((region as u64) << SHIFT) | start)
    }

    /// Releases a heap block, returning its size.
    pub fn free(&mut self, addr: Addr) -> Result<u64, MemError> {
        if addr >> SHIFT != Region::Heap as u64 {
            return Err(MemError::BadFree(addr));
        }
        self.blocks.remove(&(addr & OFFSET_MASK)).ok_or(MemError::BadFree(addr))
    }

    /// The current top of the stack, to release everything above it later.
    pub fn stack_mark(&self) -> usize {
        self.regions[Region::Stack as usize - 1].len()
    }

    /// Releases the stack above `mark`.
    pub fn stack_reset(&mut self, mark: usize) {
        self.regions[Region::Stack as usize - 1].truncate(mark);
    }

    fn locate(&self, addr: Addr, len: u64) -> Result<(usize, usize), MemError> {
        if addr == 0 {
            return Err(MemError::Nil);
        }
        let tag = addr >> SHIFT;
        if !(1..=3).contains(&tag) {
            return Err(MemError::Invalid(addr));
        }
        let region = tag as usize - 1;
        let offset = addr & OFFSET_MASK;
        let end = offset.checked_add(len).ok_or(MemError::Invalid(addr))?;
        if offset == 0 || end > self.regions[region].len() as u64 {
            return Err(MemError::Invalid(addr));
        }
        Ok((region, offset as usize))
    }

    /// Reads `len` bytes.
    pub fn read(&self, addr: Addr, len: u64) -> Result<&[u8], MemError> {
        if len == 0 {
            return Ok(&[]);
        }
        let (region, offset) = self.locate(addr, len)?;
        Ok(&self.regions[region][offset..offset + len as usize])
    }

    /// Writes bytes.
    pub fn write(&mut self, addr: Addr, bytes: &[u8]) -> Result<(), MemError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let (region, offset) = self.locate(addr, bytes.len() as u64)?;
        self.regions[region][offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Copies `len` bytes; the ranges may overlap.
    pub fn copy(&mut self, dst: Addr, src: Addr, len: u64) -> Result<(), MemError> {
        if len == 0 {
            return Ok(());
        }
        let bytes = self.read(src, len)?.to_vec();
        self.write(dst, &bytes)
    }

    /// Sets `len` bytes to `byte`.
    pub fn fill(&mut self, dst: Addr, byte: u8, len: u64) -> Result<(), MemError> {
        if len == 0 {
            return Ok(());
        }
        let (region, offset) = self.locate(dst, len)?;
        self.regions[region][offset..offset + len as usize].fill(byte);
        Ok(())
    }

    /// Reads a little-endian 64-bit word.
    pub fn read_u64(&self, addr: Addr) -> Result<u64, MemError> {
        let b = self.read(addr, 8)?;
        Ok(u64::from_le_bytes(b.try_into().map_err(|_| MemError::Invalid(addr))?))
    }

    /// Writes a little-endian 64-bit word.
    pub fn write_u64(&mut self, addr: Addr, v: u64) -> Result<(), MemError> {
        self.write(addr, &v.to_le_bytes())
    }

    /// Reads a NUL-terminated byte string.
    pub fn read_cstr(&self, addr: Addr) -> Result<Vec<u8>, MemError> {
        let (region, offset) = self.locate(addr, 1)?;
        let bytes = &self.regions[region][offset..];
        match bytes.iter().position(|b| *b == 0) {
            Some(n) => Ok(bytes[..n].to_vec()),
            None => Err(MemError::Invalid(addr)),
        }
    }

    /// Returns static, NUL-terminated storage holding `bytes`, shared by every
    /// literal with the same content.
    pub fn intern(&mut self, bytes: &[u8]) -> Result<Addr, MemError> {
        if let Some(&a) = self.strings.get(bytes) {
            return Ok(a);
        }
        let addr = self.alloc(Region::Static, bytes.len() as u64 + 1, 1)?;
        self.write(addr, bytes)?;
        self.strings.insert(bytes.to_vec(), addr);
        Ok(addr)
    }
}
