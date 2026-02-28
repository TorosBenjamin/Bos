# Introduction

Welcome to the Bos Documentation.

Bos is a hobbyist operating system kernel written in Rust. It aims to be a modern, x86_64 kernel using current Rust features and best practices.

## Project Structure

- `kernel/`: The main kernel source code.
- `kernel_api_types/`: Shared types between the kernel and userland.
- `runner/`: Helper to build and run the kernel.
- `init_task/`: Init task (first user-space process).
- `ulib/`: User space library.
- `display_server/`: Display server user task.
- `tests/`: Integration tests for the kernel.
