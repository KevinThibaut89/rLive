//! macOS helpers: native windows that host plugin editor views.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSBackingStoreType, NSEvent, NSEventModifierFlags, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CFString, CFURLPathStyle, CFURL};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::cell::Cell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

/// A plain `NSWindow` whose content view is handed to a plugin editor.
pub struct EditorWindow {
    window: Retained<NSWindow>,
    /// Hidden by us, so its being off screen doesn't mean the user closed it.
    hidden: Cell<bool>,
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
        Some(Self {
            window,
            hidden: Cell::new(false),
        })
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
        self.hidden.set(false);
        self.window.makeKeyAndOrderFront(None);
    }

    /// Take it off screen, keeping the plugin's view in it.
    pub fn hide(&self) {
        self.hidden.set(true);
        self.window.orderOut(None);
    }

    pub fn set_visible(&self, visible: bool) {
        if visible {
            self.show();
        } else {
            self.hide();
        }
    }

    pub fn set_title(&self, title: &str) {
        self.window.setTitle(&NSString::from_str(title));
    }

    pub fn resize(&self, width: f64, height: f64) {
        self.window.setContentSize(NSSize::new(width, height));
    }

    /// False once the user closed the window. Hiding it doesn't count.
    pub fn is_visible(&self) -> bool {
        self.hidden.get() || self.window.isVisible()
    }

    pub fn close(self) {
        self.window.close();
    }
}

/// Whether the Option key is held right now, even before the app has seen an event.
pub fn option_key_down() -> bool {
    NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Option)
}

/// Where a Finder alias file points. `None` for anything that isn't one.
pub fn resolve_alias(path: &Path) -> Option<PathBuf> {
    // Bookmark files start with this; checking keeps every other file away from CoreFoundation.
    let mut magic = [0u8; 4];
    if !path.is_file() {
        return None;
    }
    use std::io::Read;
    std::fs::File::open(path)
        .ok()?
        .read_exact(&mut magic)
        .ok()?;
    if &magic != b"book" {
        return None;
    }
    unsafe {
        let url = file_url(path)?;
        let data = CFURL::new_bookmark_data_from_file(None, Some(&url), std::ptr::null_mut())?;
        let mut stale = 0;
        let target = CFURL::new_by_resolving_bookmark_data(
            None,
            Some(&data),
            objc2_core_foundation::CFURLBookmarkResolutionOptions::CFBookmarkResolutionWithoutUIMask,
            None,
            None,
            &mut stale,
            std::ptr::null_mut(),
        )?;
        let text = target.file_system_path(CFURLPathStyle::CFURLPOSIXPathStyle)?;
        Some(PathBuf::from(text.to_string()))
    }
}

fn file_url(path: &Path) -> Option<objc2_core_foundation::CFRetained<CFURL>> {
    CFURL::with_file_system_path(
        None,
        Some(&CFString::from_str(&path.to_string_lossy())),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        path.is_dir(),
    )
}

/// Write a Finder alias at `alias` pointing to `target`. For tests.
#[cfg(test)]
pub fn write_alias(target: &Path, alias: &Path) -> bool {
    use objc2_core_foundation::{CFURLBookmarkCreationOptions, CFURLBookmarkFileCreationOptions};
    let (Some(target), Some(alias)) = (file_url(target), file_url(alias)) else {
        return false;
    };
    unsafe {
        let Some(data) = CFURL::new_bookmark_data(
            None,
            Some(&target),
            CFURLBookmarkCreationOptions::SuitableForBookmarkFile,
            None,
            None,
            std::ptr::null_mut(),
        ) else {
            return false;
        };
        CFURL::write_bookmark_data_to_file(
            Some(&data),
            Some(&alias),
            0 as CFURLBookmarkFileCreationOptions,
            std::ptr::null_mut(),
        )
    }
}
