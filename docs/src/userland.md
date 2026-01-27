# Userland

Bos provides a foundation for running userland applications.

## User Land Module

The `user_land` directory contains the source code for userland components.
The kernel module `kernel/src/user_land.rs` handles the transition from kernel mode to user mode.

## Shared Types

The `kernel_api_types` crate provides shared definitions (like syscall numbers and structures) that both the kernel and userland applications use to communicate.
