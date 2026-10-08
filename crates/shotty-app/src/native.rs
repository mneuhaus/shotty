//! The few AppKit calls GPUI doesn't expose: the menu bar item and overlay window tweaks.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSColor, NSEvent, NSImage, NSMenu, NSMenuItem, NSPasteboard, NSPasteboardWriting, NSScreen, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSNumber, NSString, NSURL};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ShottyMenuTarget"]
    pub struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(menuItemInvoked:))]
        fn menu_item_invoked(&self, sender: &NSMenuItem) {
            let _ = crate::events().try_send(crate::Event::Menu(sender.tag() as usize));
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

pub struct MenuEntry {
    pub title: String,
    /// Shown right-aligned, e.g. "⇧⌘4"; purely informational, the global hotkey does the work.
    pub key: Option<(String, objc2_app_kit::NSEventModifierFlags)>,
    pub separator_before: bool,
}

/// The menu bar item. Clicks on entries arrive as `Event::Menu(index)`.
pub struct StatusItem {
    item: Retained<NSStatusItem>,
    target: Retained<MenuTarget>,
    recording: Cell<bool>,
}

impl StatusItem {
    pub fn new() -> Self {
        let mtm = MainThreadMarker::new().expect("status item must be created on the main thread");
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let this = Self { item, target: MenuTarget::new(mtm), recording: Cell::new(false) };
        this.set_recording(false);
        this
    }

    pub fn set_menu(&self, entries: &[MenuEntry]) {
        let mtm = MainThreadMarker::new().expect("main thread");
        let menu = NSMenu::new(mtm);
        for (i, entry) in entries.iter().enumerate() {
            if entry.separator_before {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
            }
            let (key, mods) = entry.key.clone().unwrap_or_else(|| (String::new(), objc2_app_kit::NSEventModifierFlags::empty()));
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(&entry.title),
                    Some(sel!(menuItemInvoked:)),
                    &NSString::from_str(&key),
                )
            };
            item.setKeyEquivalentModifierMask(mods);
            unsafe { item.setTarget(Some(&self.target)) };
            item.setTag(i as isize);
            menu.addItem(&item);
        }
        self.item.setMenu(Some(&menu));
    }

    /// Idle: a template camera symbol that follows the menu bar's appearance. Recording: a red dot.
    pub fn set_recording(&self, recording: bool) {
        let mtm = MainThreadMarker::new().expect("main thread");
        self.recording.set(recording);
        let Some(button) = self.item.button(mtm) else { return };
        let symbol = if recording { "record.circle.fill" } else { "camera.viewfinder" };
        if let Some(image) =
            NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(symbol), Some(&NSString::from_str("Shotty")))
        {
            image.setTemplate(!recording);
            button.setImage(Some(&image));
        }
        button.setContentTintColor(recording.then(NSColor::systemRedColor).as_deref());
    }

    pub fn is_recording(&self) -> bool {
        self.recording.get()
    }
}

fn ns_window(window: &gpui_kit::Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
    view.window()
}

fn screen_for_display(display_id: u32, mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
    NSScreen::screens(mtm).iter().find(|screen| {
        let number = screen.deviceDescription().objectForKey(&NSString::from_str("NSScreenNumber"));
        number.and_then(|n| n.downcast::<NSNumber>().ok()).is_some_and(|n| n.unsignedIntValue() == display_id)
    })
}

/// Turns GPUI's titled popup panel into a borderless, shadowless panel covering the whole display,
/// menu bar included (GPUI has no option for that).
pub fn make_fullscreen_overlay(window: &gpui_kit::Window, display_id: u32) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(win) = ns_window(window) else { return };
    win.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
    win.setHasShadow(false);
    win.setMovable(false);
    // Screen-saver level: above the menu bar, the Dock and notification banners.
    win.setLevel(1000);
    match screen_for_display(display_id, mtm) {
        Some(screen) => win.setFrame_display(screen.frame(), true),
        None => log::warn!("no NSScreen for display {display_id}"),
    }

    win.makeKeyAndOrderFront(None);
}

/// Lets clicks pass through a window (the frame drawn around a running recording).
pub fn ignore_mouse(window: &gpui_kit::Window) {
    if let Some(win) = ns_window(window) {
        win.setIgnoresMouseEvents(true);
    }
}

/// Puts files on the clipboard as file URLs, so they paste into Finder, mail and chat apps.
pub fn copy_files_to_clipboard(paths: &[std::path::PathBuf]) {
    let urls: Vec<Retained<NSURL>> =
        paths.iter().map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()))).collect();
    let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> =
        urls.into_iter().map(ProtocolObject::from_retained).collect();
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    pasteboard.writeObjects(&NSArray::from_retained_slice(&objects));
}

/// The mouse position in global CoreGraphics points (top-left origin, y down).
pub fn mouse_location() -> (f64, f64) {
    let Some(mtm) = MainThreadMarker::new() else { return (0., 0.) };
    let p = NSEvent::mouseLocation();
    // AppKit measures from the bottom-left of the primary screen (the first in the list).
    let primary_height = NSScreen::screens(mtm).firstObject().map_or(0., |s| s.frame().size.height);
    (p.x, primary_height - p.y)
}

/// Whether the given GPUI window is AppKit's key window (receives keyboard input).
pub fn is_key_window(window: &gpui_kit::Window) -> bool {
    ns_window(window).is_some_and(|w| w.isKeyWindow())
}
