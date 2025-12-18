#![no_std]

pub mod graphics;

#[repr(u64)]
#[derive(Clone, Copy, Debug)]
pub enum SysCallNumber {
    GetBoundingBox = 0,
    DrawIter = 1,
    FillSolid = 2,
}
