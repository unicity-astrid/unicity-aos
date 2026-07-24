use super::MachineError;
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

pub(super) const ATOMIC_RAM_CHUNK_BYTES: usize = 2 * 1024 * 1024;
const WORD_BYTES: usize = size_of::<u64>();
const RESERVATION_GRANULE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AtomicReservation {
    index: usize,
    bytes: u8,
    generation: u64,
}

#[derive(Debug)]
struct AtomicRamChunk {
    words: Box<[AtomicU64]>,
    write_generations: Box<[AtomicU64]>,
}

impl AtomicRamChunk {
    fn zeroed(bytes: usize) -> Result<Self, MachineError> {
        let word_count = bytes.div_ceil(WORD_BYTES);
        let generation_count = bytes.div_ceil(RESERVATION_GRANULE_BYTES);
        let mut words = Vec::new();
        words
            .try_reserve_exact(word_count)
            .map_err(|_| MachineError::RamAllocationDenied(bytes))?;
        words.resize_with(word_count, AtomicU64::default);
        let mut write_generations = Vec::new();
        write_generations
            .try_reserve_exact(generation_count)
            .map_err(|_| MachineError::RamAllocationDenied(bytes))?;
        write_generations.resize_with(generation_count, AtomicU64::default);
        Ok(Self {
            words: words.into_boxed_slice(),
            write_generations: write_generations.into_boxed_slice(),
        })
    }

    fn clone_quiescent(&self) -> Self {
        Self {
            words: self
                .words
                .iter()
                .map(|word| AtomicU64::new(word.load(Ordering::Acquire)))
                .collect(),
            write_generations: self
                .write_generations
                .iter()
                .map(|generation| AtomicU64::new(generation.load(Ordering::Acquire)))
                .collect(),
        }
    }

    fn clear_quiescent(&mut self) {
        for word in &mut self.words {
            *word.get_mut() = 0;
        }
        for generation in &mut self.write_generations {
            *generation.get_mut() = 0;
        }
    }

    fn read_value(&self, offset: usize, bytes: u8) -> Option<u64> {
        let bytes = usize::from(bytes);
        let word_index = offset / WORD_BYTES;
        let word_offset = offset % WORD_BYTES;
        if bytes == 0 || bytes > WORD_BYTES {
            return None;
        }
        if word_offset.checked_add(bytes)? <= WORD_BYTES {
            let value = self.words.get(word_index)?.load(Ordering::Acquire);
            return Some((value >> (word_offset * 8)) & byte_mask(bytes));
        }
        let first = self.words.get(word_index)?.load(Ordering::Acquire);
        let second = self.words.get(word_index + 1)?.load(Ordering::Acquire);
        let first_bytes = WORD_BYTES - word_offset;
        let first_value = (first >> (word_offset * 8)) & byte_mask(first_bytes);
        let second_bytes = bytes - first_bytes;
        Some(first_value | ((second & byte_mask(second_bytes)) << (first_bytes * 8)))
    }

    fn write_value(&self, offset: usize, value: u64, bytes: u8) -> bool {
        let bytes = usize::from(bytes);
        let word_index = offset / WORD_BYTES;
        let word_offset = offset % WORD_BYTES;
        if bytes == 0
            || bytes > WORD_BYTES
            || word_offset
                .checked_add(bytes)
                .is_none_or(|end| end > WORD_BYTES)
        {
            return false;
        }
        let Some(word) = self.words.get(word_index) else {
            return false;
        };
        let Some(generation) = self
            .write_generations
            .get(offset / RESERVATION_GRANULE_BYTES)
        else {
            return false;
        };
        let observed = lock_generation(generation);
        let shift = word_offset * 8;
        let mask = byte_mask(bytes) << shift;
        let old = word.load(Ordering::Relaxed);
        word.store((old & !mask) | ((value << shift) & mask), Ordering::Release);
        generation.store(observed.wrapping_add(2), Ordering::Release);
        true
    }

    fn load_reserved(&self, offset: usize, bytes: u8) -> Option<(u64, u64)> {
        let bytes = usize::from(bytes);
        let word_index = offset / WORD_BYTES;
        let word_offset = offset % WORD_BYTES;
        if bytes == 0
            || bytes > WORD_BYTES
            || word_offset.checked_add(bytes)? > WORD_BYTES
            || !offset.is_multiple_of(bytes)
        {
            return None;
        }
        let word = self.words.get(word_index)?;
        let generation = self
            .write_generations
            .get(offset / RESERVATION_GRANULE_BYTES)?;
        loop {
            let before = generation.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let value = word.load(Ordering::Acquire);
            let after = generation.load(Ordering::Acquire);
            if before == after {
                let shift = word_offset * 8;
                return Some(((value >> shift) & byte_mask(bytes), before));
            }
        }
    }

    fn store_conditional(
        &self,
        offset: usize,
        value: u64,
        bytes: u8,
        expected_generation: u64,
    ) -> bool {
        let bytes = usize::from(bytes);
        let word_index = offset / WORD_BYTES;
        let word_offset = offset % WORD_BYTES;
        if expected_generation & 1 != 0
            || bytes == 0
            || bytes > WORD_BYTES
            || word_offset
                .checked_add(bytes)
                .is_none_or(|end| end > WORD_BYTES)
            || !offset.is_multiple_of(bytes)
        {
            return false;
        }
        let Some(word) = self.words.get(word_index) else {
            return false;
        };
        let Some(generation) = self
            .write_generations
            .get(offset / RESERVATION_GRANULE_BYTES)
        else {
            return false;
        };
        if generation
            .compare_exchange(
                expected_generation,
                expected_generation.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        let shift = word_offset * 8;
        let mask = byte_mask(bytes) << shift;
        let old = word.load(Ordering::Relaxed);
        word.store((old & !mask) | ((value << shift) & mask), Ordering::Release);
        generation.store(expected_generation.wrapping_add(2), Ordering::Release);
        true
    }

    fn update_value(
        &self,
        offset: usize,
        value: impl FnOnce(u64) -> u64,
        bytes: u8,
    ) -> Option<u64> {
        let bytes = usize::from(bytes);
        let word_index = offset / WORD_BYTES;
        let word_offset = offset % WORD_BYTES;
        if bytes == 0
            || bytes > WORD_BYTES
            || word_offset.checked_add(bytes)? > WORD_BYTES
            || !offset.is_multiple_of(bytes)
        {
            return None;
        }
        let word = self.words.get(word_index)?;
        let generation = self
            .write_generations
            .get(offset / RESERVATION_GRANULE_BYTES)?;
        let observed = lock_generation(generation);
        let shift = word_offset * 8;
        let mask = byte_mask(bytes) << shift;
        let stored = word.load(Ordering::Relaxed);
        let old = (stored >> shift) & byte_mask(bytes);
        let new = value(old);
        word.store(
            (stored & !mask) | ((new << shift) & mask),
            Ordering::Release,
        );
        generation.store(observed.wrapping_add(2), Ordering::Release);
        Some(old)
    }

    fn read_byte(&self, offset: usize) -> Option<u8> {
        self.read_value(offset, 1).map(|value| value as u8)
    }

    fn write_bytes_quiescent(&mut self, offset: usize, bytes: &[u8]) -> bool {
        let Some(end) = offset
            .checked_add(bytes.len())
            .filter(|end| *end <= self.words.len() * WORD_BYTES)
        else {
            return false;
        };
        let mut source = 0;
        let mut destination = offset;
        while destination < end {
            let word_index = destination / WORD_BYTES;
            let word_offset = destination % WORD_BYTES;
            let copy_len = (end - destination).min(WORD_BYTES - word_offset);
            let shift = word_offset * 8;
            let mask = byte_mask(copy_len) << shift;
            let mut value = 0_u64;
            for (byte_index, byte) in bytes[source..source + copy_len].iter().enumerate() {
                value |= u64::from(*byte) << (byte_index * 8);
            }
            let slot = self.words[word_index].get_mut();
            *slot = (*slot & !mask) | ((value << shift) & mask);
            destination += copy_len;
            source += copy_len;
        }
        true
    }
}

fn byte_mask(bytes: usize) -> u64 {
    if bytes == WORD_BYTES {
        u64::MAX
    } else {
        (1_u64 << (bytes * 8)) - 1
    }
}

fn lock_generation(generation: &AtomicU64) -> u64 {
    let mut observed = generation.load(Ordering::Acquire);
    loop {
        if observed & 1 != 0 {
            std::hint::spin_loop();
            observed = generation.load(Ordering::Acquire);
            continue;
        }
        match generation.compare_exchange_weak(
            observed,
            observed.wrapping_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return observed,
            Err(actual) => observed = actual,
        }
    }
}

#[derive(Debug)]
pub(super) struct AtomicGuestRam {
    chunks: Box<[OnceLock<Result<AtomicRamChunk, MachineError>>]>,
    len: usize,
}

impl Clone for AtomicGuestRam {
    fn clone(&self) -> Self {
        let mut clone = Self::zeroed(self.len).expect("existing RAM metadata remains admissible");
        for (destination, source) in clone.chunks.iter_mut().zip(self.chunks.iter()) {
            match source.get() {
                Some(Ok(chunk)) => {
                    destination
                        .set(Ok(chunk.clone_quiescent()))
                        .expect("fresh atomic RAM chunk");
                }
                Some(Err(_)) => {
                    destination
                        .set(Err(MachineError::RamAllocationDenied(
                            ATOMIC_RAM_CHUNK_BYTES,
                        )))
                        .expect("fresh failed atomic RAM chunk");
                }
                None => {}
            }
        }
        clone
    }
}

impl AtomicGuestRam {
    pub(super) fn zeroed(len: usize) -> Result<Self, MachineError> {
        let chunk_count = len.div_ceil(ATOMIC_RAM_CHUNK_BYTES);
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(chunk_count)
            .map_err(|_| MachineError::RamAllocationDenied(len))?;
        chunks.resize_with(chunk_count, OnceLock::new);
        Ok(Self {
            chunks: chunks.into_boxed_slice(),
            len,
        })
    }

    pub(super) fn fill_zero(&mut self) {
        for chunk in &mut self.chunks {
            if let Some(Ok(chunk)) = chunk.get_mut() {
                chunk.clear_quiescent();
            }
        }
    }

    pub(super) fn byte(&self, index: usize) -> Option<u8> {
        if index >= self.len {
            return None;
        }
        let chunk_index = index / ATOMIC_RAM_CHUNK_BYTES;
        let chunk_offset = index % ATOMIC_RAM_CHUNK_BYTES;
        match self.chunks[chunk_index].get() {
            None => Some(0),
            Some(Ok(chunk)) => chunk.read_byte(chunk_offset),
            Some(Err(_)) => None,
        }
    }

    pub(super) fn read_value(&self, index: usize, bytes: u8) -> Option<u64> {
        let byte_count = usize::from(bytes);
        let end = index.checked_add(byte_count)?;
        if byte_count == 0 || byte_count > WORD_BYTES || end > self.len {
            return None;
        }
        let chunk_index = index / ATOMIC_RAM_CHUNK_BYTES;
        let chunk_offset = index % ATOMIC_RAM_CHUNK_BYTES;
        if chunk_offset + byte_count <= ATOMIC_RAM_CHUNK_BYTES {
            return match self.chunks[chunk_index].get() {
                None => Some(0),
                Some(Ok(chunk)) => chunk.read_value(chunk_offset, bytes),
                Some(Err(_)) => None,
            };
        }
        let mut value = 0_u64;
        for shift in 0..byte_count {
            value |= u64::from(self.byte(index + shift)?) << (shift * 8);
        }
        Some(value)
    }

    pub(super) fn write_value(&self, index: usize, value: u64, bytes: u8) -> bool {
        let byte_count = usize::from(bytes);
        let Some(end) = index.checked_add(byte_count) else {
            return false;
        };
        if byte_count == 0
            || byte_count > WORD_BYTES
            || end > self.len
            || !index.is_multiple_of(byte_count)
        {
            return false;
        }
        let chunk_index = index / ATOMIC_RAM_CHUNK_BYTES;
        let chunk_offset = index % ATOMIC_RAM_CHUNK_BYTES;
        if chunk_offset + byte_count > ATOMIC_RAM_CHUNK_BYTES {
            return false;
        }
        if value & byte_mask(byte_count) == 0 && self.chunks[chunk_index].get().is_none() {
            return true;
        }
        match self.chunk(chunk_index) {
            Some(chunk) => chunk.write_value(chunk_offset, value, bytes),
            None => false,
        }
    }

    pub(super) fn set_byte(&self, index: usize, value: u8) -> bool {
        if index >= self.len {
            return false;
        }
        let chunk_index = index / ATOMIC_RAM_CHUNK_BYTES;
        let chunk_offset = index % ATOMIC_RAM_CHUNK_BYTES;
        if value == 0 && self.chunks[chunk_index].get().is_none() {
            return true;
        }
        match self.chunk(chunk_index) {
            Some(chunk) => chunk.write_value(chunk_offset, u64::from(value), 1),
            None => false,
        }
    }

    pub(super) fn load_reserved(
        &self,
        index: usize,
        bytes: u8,
    ) -> Option<(u64, AtomicReservation)> {
        let byte_count = usize::from(bytes);
        let end = index.checked_add(byte_count)?;
        if byte_count == 0
            || byte_count > WORD_BYTES
            || end > self.len
            || !index.is_multiple_of(byte_count)
        {
            return None;
        }
        let chunk_index = index / ATOMIC_RAM_CHUNK_BYTES;
        let chunk_offset = index % ATOMIC_RAM_CHUNK_BYTES;
        let (value, generation) = match self.chunks[chunk_index].get() {
            None => (0, 0),
            Some(Ok(chunk)) => chunk.load_reserved(chunk_offset, bytes)?,
            Some(Err(_)) => return None,
        };
        Some((
            value,
            AtomicReservation {
                index,
                bytes,
                generation,
            },
        ))
    }

    pub(super) fn store_conditional(
        &self,
        reservation: AtomicReservation,
        index: usize,
        value: u64,
        bytes: u8,
    ) -> bool {
        if reservation.index != index || reservation.bytes != bytes {
            return false;
        }
        let byte_count = usize::from(bytes);
        let Some(end) = index.checked_add(byte_count) else {
            return false;
        };
        if byte_count == 0
            || byte_count > WORD_BYTES
            || end > self.len
            || !index.is_multiple_of(byte_count)
        {
            return false;
        }
        let chunk_index = index / ATOMIC_RAM_CHUNK_BYTES;
        let chunk_offset = index % ATOMIC_RAM_CHUNK_BYTES;
        self.chunk(chunk_index).is_some_and(|chunk| {
            chunk.store_conditional(chunk_offset, value, bytes, reservation.generation)
        })
    }

    pub(super) fn update_value(
        &self,
        index: usize,
        bytes: u8,
        value: impl FnOnce(u64) -> u64,
    ) -> Option<u64> {
        let byte_count = usize::from(bytes);
        let end = index.checked_add(byte_count)?;
        if byte_count == 0
            || byte_count > WORD_BYTES
            || end > self.len
            || !index.is_multiple_of(byte_count)
        {
            return None;
        }
        let chunk_index = index / ATOMIC_RAM_CHUNK_BYTES;
        let chunk_offset = index % ATOMIC_RAM_CHUNK_BYTES;
        self.chunk(chunk_index)?
            .update_value(chunk_offset, value, bytes)
    }

    pub(super) fn copy_from_slice(&mut self, start: usize, source: &[u8]) -> bool {
        let Some(end) = start
            .checked_add(source.len())
            .filter(|end| *end <= self.len)
        else {
            return false;
        };
        let mut destination = start;
        let mut source_offset = 0;
        while destination < end {
            let chunk_index = destination / ATOMIC_RAM_CHUNK_BYTES;
            let chunk_offset = destination % ATOMIC_RAM_CHUNK_BYTES;
            let copy_len = (end - destination).min(ATOMIC_RAM_CHUNK_BYTES - chunk_offset);
            let source_slice = &source[source_offset..source_offset + copy_len];
            if source_slice.iter().any(|byte| *byte != 0) {
                let Some(chunk) = self.chunk_mut(chunk_index) else {
                    return false;
                };
                if !chunk.write_bytes_quiescent(chunk_offset, source_slice) {
                    return false;
                }
            } else if let Some(Ok(chunk)) = self.chunks[chunk_index].get_mut()
                && !chunk.write_bytes_quiescent(chunk_offset, source_slice)
            {
                return false;
            }
            destination += copy_len;
            source_offset += copy_len;
        }
        true
    }

    pub(super) fn copy_to_vec(&self, range: std::ops::Range<usize>) -> Option<Vec<u8>> {
        if range.start > range.end || range.end > self.len {
            return None;
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(range.len()).ok()?;
        bytes.resize(range.len(), 0);
        for (destination, source) in bytes.iter_mut().zip(range) {
            *destination = self.byte(source)?;
        }
        Some(bytes)
    }

    pub(super) fn nonzero_pages(&self, page_bytes: usize) -> Vec<(usize, Vec<u8>)> {
        debug_assert!(page_bytes > 0);
        debug_assert_eq!(self.len % page_bytes, 0);
        debug_assert_eq!(ATOMIC_RAM_CHUNK_BYTES % page_bytes, 0);
        let pages_per_chunk = ATOMIC_RAM_CHUNK_BYTES / page_bytes;
        let mut pages = Vec::new();
        for (chunk_index, chunk) in self.chunks.iter().enumerate() {
            let Some(Ok(_)) = chunk.get() else {
                continue;
            };
            let chunk_start = chunk_index * ATOMIC_RAM_CHUNK_BYTES;
            let chunk_bytes = (self.len - chunk_start).min(ATOMIC_RAM_CHUNK_BYTES);
            for page_in_chunk in 0..chunk_bytes.div_ceil(page_bytes) {
                let index = chunk_index * pages_per_chunk + page_in_chunk;
                let start = index * page_bytes;
                let bytes = self.copy_to_vec(start..(start + page_bytes).min(self.len));
                let Some(bytes) = bytes.filter(|bytes| bytes.iter().any(|byte| *byte != 0)) else {
                    continue;
                };
                pages.push((index, bytes));
            }
        }
        pages
    }

    fn chunk(&self, index: usize) -> Option<&AtomicRamChunk> {
        self.chunks
            .get(index)?
            .get_or_init(|| {
                let start = index * ATOMIC_RAM_CHUNK_BYTES;
                AtomicRamChunk::zeroed((self.len - start).min(ATOMIC_RAM_CHUNK_BYTES))
            })
            .as_ref()
            .ok()
    }

    fn chunk_mut(&mut self, index: usize) -> Option<&mut AtomicRamChunk> {
        let len = self.len;
        let cell = self.chunks.get_mut(index)?;
        if cell.get().is_none() {
            let start = index * ATOMIC_RAM_CHUNK_BYTES;
            cell.set(AtomicRamChunk::zeroed(
                (len - start).min(ATOMIC_RAM_CHUNK_BYTES),
            ))
            .expect("empty atomic RAM chunk");
        }
        cell.get_mut()?.as_mut().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn demand_zero_chunks_preserve_bounds_and_little_endian_values() {
        let mut ram =
            AtomicGuestRam::zeroed(ATOMIC_RAM_CHUNK_BYTES * 2).expect("atomic RAM metadata");
        assert_eq!(ram.read_value(0, 8), Some(0));
        assert_eq!(ram.byte(ATOMIC_RAM_CHUNK_BYTES + 7), Some(0));
        assert_eq!(ram.byte(ram.len), None);
        assert!(ram.write_value(8, 0x1122_3344_5566_7788, 8));
        assert_eq!(ram.read_value(8, 8), Some(0x1122_3344_5566_7788));
        assert!(ram.set_byte(9, 0xcc));
        assert_eq!(ram.read_value(8, 8), Some(0x1122_3344_5566_cc88));
        assert!(ram.write_value(10, 0xaabb, 2));
        assert_eq!(ram.read_value(8, 8), Some(0x1122_3344_aabb_cc88));
        assert!(!ram.write_value(3, 1, 4));
        assert!(!ram.write_value(ram.len, 1, 1));

        assert!(ram.copy_from_slice(ATOMIC_RAM_CHUNK_BYTES - 3, &[1, 2, 3, 4, 5, 6]));
        assert_eq!(
            ram.copy_to_vec(ATOMIC_RAM_CHUNK_BYTES - 3..ATOMIC_RAM_CHUNK_BYTES + 3),
            Some(vec![1, 2, 3, 4, 5, 6])
        );
        assert!(ram.copy_from_slice(4096, &vec![7; 4096]));
        assert!(
            ram.nonzero_pages(4096)
                .iter()
                .any(|(index, bytes)| *index == 1 && bytes.iter().all(|byte| *byte == 7))
        );
        ram.fill_zero();
        assert_eq!(ram.read_value(8, 8), Some(0));
    }

    #[test]
    fn aligned_words_never_tear_under_concurrent_access() {
        let ram = Arc::new(AtomicGuestRam::zeroed(4096).expect("atomic RAM"));
        let writer = {
            let ram = Arc::clone(&ram);
            std::thread::spawn(move || {
                for iteration in 0..100_000_u64 {
                    let value = if iteration & 1 == 0 {
                        0xaaaa_aaaa_aaaa_aaaa
                    } else {
                        0x5555_5555_5555_5555
                    };
                    assert!(ram.write_value(0, value, 8));
                }
            })
        };
        for _ in 0..100_000 {
            assert!(matches!(
                ram.read_value(0, 8),
                Some(0 | 0xaaaa_aaaa_aaaa_aaaa | 0x5555_5555_5555_5555)
            ));
        }
        writer.join().expect("atomic RAM writer");
    }

    #[test]
    fn disjoint_reservation_granules_make_concurrent_progress() {
        let ram = Arc::new(AtomicGuestRam::zeroed(4096).expect("atomic RAM"));
        let handles = (0..8_usize)
            .map(|worker| {
                let ram = Arc::clone(&ram);
                std::thread::spawn(move || {
                    let address = worker * RESERVATION_GRANULE_BYTES;
                    for value in 1..=10_000_u64 {
                        assert!(ram.write_value(address, value, 8));
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("disjoint RAM writer");
        }
        for worker in 0..8 {
            assert_eq!(
                ram.read_value(worker * RESERVATION_GRANULE_BYTES, 8),
                Some(10_000)
            );
        }
    }

    #[test]
    fn reservations_ignore_disjoint_stores_and_reject_overlapping_stores() {
        let ram = AtomicGuestRam::zeroed(4096).expect("atomic RAM");
        assert!(ram.write_value(0, 10, 8));
        let (value, reservation) = ram.load_reserved(0, 8).expect("reservation");
        assert_eq!(value, 10);
        assert!(ram.write_value(RESERVATION_GRANULE_BYTES, 99, 8));
        assert!(ram.store_conditional(reservation, 0, 11, 8));
        assert_eq!(ram.read_value(0, 8), Some(11));

        let (_, reservation) = ram.load_reserved(0, 8).expect("second reservation");
        assert!(ram.write_value(8, 12, 8));
        assert!(!ram.store_conditional(reservation, 0, 13, 8));
        assert_eq!(ram.read_value(0, 8), Some(11));
    }

    #[test]
    fn contended_lr_sc_loops_eventually_commit_every_increment() {
        let ram = Arc::new(AtomicGuestRam::zeroed(4096).expect("atomic RAM"));
        let handles = (0..8)
            .map(|_| {
                let ram = Arc::clone(&ram);
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        loop {
                            let (value, reservation) =
                                ram.load_reserved(0, 8).expect("load reservation");
                            if ram.store_conditional(reservation, 0, value.wrapping_add(1), 8) {
                                break;
                            }
                            std::hint::spin_loop();
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("contended LR/SC worker");
        }
        assert_eq!(ram.read_value(0, 8), Some(16_000));
    }

    #[test]
    fn atomic_updates_return_one_total_order() {
        let ram = Arc::new(AtomicGuestRam::zeroed(4096).expect("atomic RAM"));
        let handles = (0..8)
            .map(|_| {
                let ram = Arc::clone(&ram);
                std::thread::spawn(move || {
                    (0..2_000)
                        .map(|_| {
                            ram.update_value(0, 8, |value| value.wrapping_add(1))
                                .expect("atomic update")
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let mut old_values = handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("atomic update worker"))
            .collect::<Vec<_>>();
        old_values.sort_unstable();
        assert_eq!(old_values, (0..16_000).collect::<Vec<_>>());
        assert_eq!(ram.read_value(0, 8), Some(16_000));
    }
}
