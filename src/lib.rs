//! Async filesystem API shaped like `tokio::fs`, built directly on
//! `wasi:filesystem` 0.3 (wasip3). No runtime dependency: every function is a
//! plain `Future`, driven by whatever executor the component uses.
//!
//! Differences from `tokio::fs`:
//! - `File` exposes inherent async methods instead of `AsyncRead`/`AsyncWrite`.
//! - Paths must fall under a WASI preopen; relative paths use the `.` preopen
//!   (or `/` as a fallback).
//! - `canonicalize` is lexical (no symlink resolution); there is no creation time
//!   and no `set_permissions`.
//! - `read_dir` fetches all entries eagerly.
//!
//! ```ignore
//! let text = wasip3_fs::read_to_string("/data/config.toml").await?;
//! wasip3_fs::create_dir_all("/data/out/logs").await?;
//! wasip3_fs::write("/data/out/logs/run.txt", text).await?;
//! ```

#![forbid(unsafe_code)]

mod file;
mod metadata;
mod ops;
mod sys;

pub use file::{File, OpenOptions};
pub use metadata::{FileType, Metadata};
pub use ops::{
    canonicalize, copy, create_dir, create_dir_all, hard_link, metadata, read, read_dir, read_link,
    read_to_string, remove_dir, remove_dir_all, remove_file, rename, symlink, symlink_metadata,
    try_exists, write, DirBuilder, DirEntry, ReadDir,
};
