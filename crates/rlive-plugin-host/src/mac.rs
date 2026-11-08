//! macOS helpers: native windows that host plugin editor views.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::ffi::c_void;

/// A plain `NSWindow` whose content view is handed to a plugin editor.
pub struct EditorWindow {
    window: Retained<NSWindow>,
}

impl EditorWindow {
    /// Create (but do not show) an editor window. Must be called on the main thread.
    pub fn new(title: &str, width: f64, height: f64, resizable: bool) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let mut style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;
        if resizable {
            style |= NSWindowStyleMask::Resizable;
        }
        let rect = NSRect::new(
            NSPoint::new(200.0, 200.0),
            NSSize::new(width.max(50.0), height.max(50.0)),
        );
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // We own the window; AppKit must not release it behind our back when the user closes it.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        window.center();
        Some(Self { window })
    }

    /// The window's content view as a raw `NSView*`, for passing to plugin APIs.
    pub fn ns_view(&self) -> *mut c_void {
        self.content_view()
            .map(|v| Retained::as_ptr(&v) as *mut c_void)
            .unwrap_or(std::ptr::null_mut())
    }

    pub fn content_view(&self) -> Option<Retained<NSView>> {
        self.window.contentView()
    }

    /// Add a plugin-created view as the window content.
    pub fn set_content(&self, view: &NSView) {
        let size = view.frame().size;
        self.window.setContentView(Some(view));
        self.window.setContentSize(size);
    }

    pub fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn resize(&self, width: f64, height: f64) {
        self.window.setContentSize(NSSize::new(width, height));
    }

    /// False once the user closed the window.
    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    pub fn close(self) {
        self.window.close();
    }
}
