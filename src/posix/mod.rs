//! # POSIX Layer
//!
//! This module provides POSIX-like functionality for OpenV, including
//! process management, ELF loading, spawning, user-space management,
//! and wait/exit handling.
//!
//! ## Overview
//!
//! OpenV implements a subset of the POSIX API in user-space servers.
//! The kernel provides the underlying primitives (processes, IPC,
//! page tables), and the POSIX layer provides:
//!
//! - **Process management**: Creation, termination, and lifecycle of
//!    processes.
//!  - **ELF loading**: Loading and executing ELF binaries.
//!  - **Spawning**: Creating new processes from path names.
//!  - **User management**: User/group IDs and related functions.
//!  - **Wait/exit**: Process synchronization primitives.
//!
//! ## Submodules
//!
//! - [`elf`]: ELF binary parsing and loading.
//!  - [`process`]: Process management (see [`process::Process`], [`process::schedule`]).
//!  - [`spawn`]: Process spawning (see [`spawn::posix_spawn`]).
//!  - [`user`]: User/group ID management.
//!  - [`wait`]: Wait/exit handling.
//!
//! ## Architecture
//!
//! OpenV's POSIX layer is primarily implemented in user-space servers
//! (such as `pm-server` and `vfs-server`). The kernel provides only
//! the most basic primitives.

/// ELF binary parsing and loading.
pub mod elf;
/// Process management.
pub mod process;
/// Process spawning.
pub mod spawn;
/// User/group ID management.
pub mod user;
/// Wait/exit handling.
pub mod wait;
