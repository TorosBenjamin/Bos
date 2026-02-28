use crate::memory::physical_memory::MemoryType;

#[derive(Debug)]
pub enum FreeError {
    /// The frame was not found in the memory map
    FrameNotAllocated,

    /// The frame exists but has a different MemoryType
    WrongMemoryType {
        expected: MemoryType,
        found: MemoryType,
    },
}