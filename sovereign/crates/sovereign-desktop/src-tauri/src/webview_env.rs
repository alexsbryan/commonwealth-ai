// SPDX-License-Identifier: AGPL-3.0-or-later
//! WebKitGTK environment defaults, decided before GTK initialises.
//!
//! Inside a podman/toolbox container WebKitGTK's GPU rasteriser renders into
//! buffers the host compositor never shows: the window keeps its first,
//! pre-compositing paint (the body background) while the DOM underneath is
//! fully mounted and live. Measured 2026-09-22 on the Strix Halo, toolbox
//! `sovereign-vulkan` (webkit2gtk4.1 2.52.3, Mesa 25.3.6; host Mesa 26.1.8):
//! an in-page probe showed `app-chrome` laid out at 1024x720, opacity 1,
//! hit-testable, animations ticking — and the window stayed blank purple.
//! The same binary with `WEBKIT_SKIA_ENABLE_CPU_RENDERING=1` rendered the
//! full app; the host's own WebKit painted the same dev server correctly.
//! `WEBKIT_DISABLE_COMPOSITING_MODE`, `WEBKIT_DISABLE_DMABUF_RENDERER` and
//! `GDK_BACKEND=x11` did not help.
//!
//! So in a container we default Skia to CPU rasterisation. An explicit value
//! in the environment always wins (set it to `0` to test the GPU path).

use std::path::Path;

const CPU_RENDERING: &str = "WEBKIT_SKIA_ENABLE_CPU_RENDERING";
/// Written by podman (and so toolbox) into every container it starts.
const CONTAINER_MARKER: &str = "/run/.containerenv";

/// What `apply` decided — logged once tracing is up, since this runs before it.
#[derive(Debug)]
pub enum WebviewRaster {
    /// Not in a container; WebKit picks its own renderer.
    Default,
    /// The operator set the variable; left as given.
    Explicit(String),
    /// In a container with no explicit value; CPU rasterisation defaulted on.
    CpuInContainer,
}

/// Must run before any thread is spawned and before GTK initialises.
pub fn apply() -> WebviewRaster {
    decide(
        std::env::var(CPU_RENDERING).ok(),
        Path::new(CONTAINER_MARKER).exists(),
        |v| std::env::set_var(CPU_RENDERING, v),
    )
}

fn decide(explicit: Option<String>, in_container: bool, set: impl FnOnce(&str)) -> WebviewRaster {
    match (explicit, in_container) {
        (Some(v), _) => WebviewRaster::Explicit(v),
        (None, true) => {
            set("1");
            WebviewRaster::CpuInContainer
        }
        (None, false) => WebviewRaster::Default,
    }
}

pub fn log(decision: &WebviewRaster) {
    match decision {
        WebviewRaster::Default => {
            tracing::debug!(target: "bootstrap", "webview raster: WebKit default (not in a container)")
        }
        WebviewRaster::Explicit(v) => {
            tracing::info!(target: "bootstrap", value = %v, "webview raster: {CPU_RENDERING} set explicitly")
        }
        WebviewRaster::CpuInContainer => tracing::info!(
            target: "bootstrap",
            "webview raster: container detected ({CONTAINER_MARKER}); {CPU_RENDERING}=1 \
             (GPU raster paints a blank window here — set {CPU_RENDERING}=0 to override)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_without_explicit_value_defaults_cpu_raster() {
        let mut set = None;
        let d = decide(None, true, |v| set = Some(v.to_owned()));
        assert!(matches!(d, WebviewRaster::CpuInContainer));
        assert_eq!(set.as_deref(), Some("1"));
    }

    #[test]
    fn explicit_value_wins_in_container() {
        let d = decide(Some("0".into()), true, |_| panic!("must not overwrite"));
        assert!(matches!(d, WebviewRaster::Explicit(v) if v == "0"));
    }

    #[test]
    fn host_leaves_webkit_default() {
        let d = decide(None, false, |_| panic!("must not set on host"));
        assert!(matches!(d, WebviewRaster::Default));
    }
}
