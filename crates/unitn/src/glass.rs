//! Frosted-glass window background on macOS: the system's own blur
//! material (NSVisualEffectView), as used by Finder and Music sidebars.
//!
//! The blur view must sit *behind* the view Slint draws into. A subview of
//! that view would always be drawn on top of it (and hide the whole UI), so
//! the blur view is added to the window's frame view as a sibling placed
//! below the content view.

use crate::AppWindow;

#[cfg(target_os = "macos")]
mod mac {
    use objc2::encode::{Encode, Encoding};
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }
    unsafe impl Encode for CGPoint {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for CGSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for CGRect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
    }

    const MATERIAL_UNDER_WINDOW_BACKGROUND: isize = 21;
    const BLENDING_BEHIND_WINDOW: isize = 0;
    const STATE_ACTIVE: isize = 1;
    const RESIZE_WIDTH_AND_HEIGHT: usize = 2 | 16;
    const ORDER_BELOW: isize = -1;

    /// `ns_view` is the content view winit created (Slint draws into it).
    pub unsafe fn apply(ns_view: *mut std::ffi::c_void, on: bool) -> Result<(), &'static str> {
        let content = ns_view as *mut AnyObject;
        if content.is_null() {
            return Err("no view");
        }
        let content: &AnyObject = &*content;
        let frame_view: Option<Retained<AnyObject>> = msg_send![content, superview];
        let frame_view = frame_view.ok_or("view has no superview")?;

        // Remove a blur view added earlier (ours are direct children of the frame view).
        let subviews: Retained<AnyObject> = msg_send![&*frame_view, subviews];
        let count: usize = msg_send![&*subviews, count];
        for i in 0..count {
            let v: Retained<AnyObject> = msg_send![&*subviews, objectAtIndex: i];
            let is_effect: bool = msg_send![&*v, isKindOfClass: class!(NSVisualEffectView)];
            if is_effect {
                let _: () = msg_send![&*v, removeFromSuperview];
            }
        }
        if !on {
            return Ok(());
        }

        let bounds: CGRect = msg_send![&*frame_view, bounds];
        let effect: Allocated<AnyObject> = msg_send![class!(NSVisualEffectView), alloc];
        let effect: Retained<AnyObject> = msg_send![effect, initWithFrame: bounds];
        let _: () = msg_send![&*effect, setMaterial: MATERIAL_UNDER_WINDOW_BACKGROUND];
        let _: () = msg_send![&*effect, setBlendingMode: BLENDING_BEHIND_WINDOW];
        let _: () = msg_send![&*effect, setState: STATE_ACTIVE];
        let _: () = msg_send![&*effect, setAutoresizingMask: RESIZE_WIDTH_AND_HEIGHT];
        let _: () = msg_send![&*frame_view, addSubview: &*effect, positioned: ORDER_BELOW, relativeTo: content];
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub fn apply(ui: &AppWindow, on: bool) {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;
    use slint::ComponentHandle;
    use unitn_core::log::log;

    let view = ui
        .window()
        .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr()),
            _ => None,
        })
        .flatten();
    let Some(view) = view else {
        log("ui: frosted glass: no native window yet");
        return;
    };
    match unsafe { mac::apply(view, on) } {
        Ok(()) => log(format!("ui: frosted glass {}", if on { "on" } else { "off" })),
        Err(e) => log(format!("ui: frosted glass failed: {e}")),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn apply(_ui: &AppWindow, _on: bool) {}

/// Whether this platform has the frosted-glass background.
pub const AVAILABLE: bool = cfg!(target_os = "macos");
