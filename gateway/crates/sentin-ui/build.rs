// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Link the application icon and the version block into `sentin-ui.exe`.
//!
//! This is what makes Explorer, the taskbar and the Start Menu draw the shield on the file rather
//! than the blank sheet Windows gives an executable carrying no icon resource. The window's own
//! icon is a different mechanism entirely - `main.rs` hands eframe raw pixels - because a running
//! window and a file sitting on a desktop are drawn by different parts of Windows and neither
//! falls back to the other.
//!
//! `winresource` is a plain build-dependency rather than a `cfg(windows)` one on purpose. Cargo
//! resolves target-specific build-dependencies against the **host**, so a `cfg(windows)` entry
//! would be absent exactly where it is needed: the release binaries are cross-compiled on Linux
//! with mingw, and this file would then fail to compile for the one build that ships.

fn main() {
    println!("cargo:rerun-if-changed=../../../packaging/windows/sentin-npu.ico");
    println!("cargo:rerun-if-changed=build.rs");

    // The target, not the host. Building for Linux must not reach for a resource compiler.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("../../../packaging/windows/sentin-npu.ico");
    resource.set("FileDescription", "Sentin-NPU console");
    resource.set("ProductName", "Sentin-NPU");
    resource.set("LegalCopyright", "Copyright 2026 Grzegorz Oleksy");

    // A missing resource compiler must not fail the build. The icon is a courtesy to whoever looks
    // at the file in Explorer; the console works identically without it, and refusing to build
    // would turn that courtesy into a new requirement on every machine that compiles this project.
    if let Err(e) = resource.compile() {
        println!("cargo:warning=no icon resource linked into sentin-ui.exe: {e}");
    }
}
