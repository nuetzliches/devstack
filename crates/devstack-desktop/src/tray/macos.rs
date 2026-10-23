//! macOS: a status item in the menu bar (ADR-0060), from the menu bar client
//! contributed in #248. AppKit is confined to the main thread; no request
//! of the owner runs from a delegate or timer callback. The process makes
//! itself an accessory application, so it has no Dock icon. The icon is a
//! template image, so it follows the menu bar's appearance, tinted in the
//! state's colour.

use std::cell::{Cell, RefCell};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSColor, NSEvent, NSEventModifierFlags,
    NSEventType, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{
    NSData, NSPoint, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer, ns_string,
};

use super::{Entry, Menu, QUIT, Tone};

pub fn unavailable() -> Option<String> {
    None
}

struct Ivars {
    updates: Receiver<Menu>,
    actions: Sender<String>,
    status_item: Retained<NSStatusItem>,
    shown: RefCell<Option<Menu>>,
    pending: RefCell<Option<Menu>>,
    tracking: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Every callback and
    // field is confined to the AppKit main thread; this class has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    // SAFETY: The optional protocol callbacks have AppKit's declared signatures.
    unsafe impl NSMenuDelegate for MenuTarget {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.ivars().tracking.set(true);
        }

        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            // The next timer tick applies a pending snapshot, after AppKit has
            // finished dispatching the action for the selected menu item.
            self.ivars().tracking.set(false);
        }
    }

    impl MenuTarget {
        #[unsafe(method(performMenuAction:))]
        fn perform_menu_action(&self, item: &NSMenuItem) {
            let Some(object) = item.representedObject() else {
                return;
            };
            let Some(id) = object.downcast_ref::<NSString>() else {
                return;
            };
            let id = id.to_string();
            let quitting = id == QUIT;
            if self.ivars().actions.send(id).is_err() || quitting {
                self.stop();
            }
        }

        #[unsafe(method(pollUpdates:))]
        fn poll_updates(&self, _timer: &NSTimer) {
            autoreleasepool(|_| self.poll());
        }
    }
);

impl MenuTarget {
    fn new(
        mtm: MainThreadMarker,
        status_item: Retained<NSStatusItem>,
        updates: Receiver<Menu>,
        actions: Sender<String>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            updates,
            actions,
            status_item,
            shown: RefCell::new(None),
            pending: RefCell::new(None),
            tracking: Cell::new(false),
        });
        // SAFETY: The superclass is NSObject and the instance's fields have
        // been initialized before invoking its designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn poll(&self) {
        // Coalesce snapshots and bound work per event even if a producer sends
        // faster than AppKit can draw. There is never a blocking channel read.
        for _ in 0..32 {
            match self.ivars().updates.try_recv() {
                Ok(menu) => *self.ivars().pending.borrow_mut() = Some(menu),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.stop();
                    return;
                }
            }
        }
        // Keep the open menu and its action IDs stable under the pointer.
        // The timer runs in common modes, including menu tracking, but only
        // replaces a menu once its tracking session has ended.
        if !self.ivars().tracking.get() {
            let pending = self.ivars().pending.borrow_mut().take();
            if let Some(menu) = pending {
                self.apply(menu);
            }
        }
    }

    fn apply(&self, snapshot: Menu) {
        if self.ivars().shown.borrow().as_ref() == Some(&snapshot) {
            return;
        }
        if let Some(button) = self.ivars().status_item.button(self.mtm()) {
            button.setToolTip(Some(&NSString::from_str(&snapshot.tooltip)));
            // A template image takes the tint; none follows the menu bar.
            let tint = match snapshot.tone {
                Tone::Idle | Tone::Ok => None,
                Tone::Busy => Some(NSColor::systemOrangeColor()),
                Tone::Attention => Some(NSColor::systemRedColor()),
            };
            button.setContentTintColor(tint.as_deref());
        }
        let menu = build_menu(self, &snapshot.entries);
        // NSMenu's delegate and NSMenuItem's target are weak; `run` retains
        // this object until the menu is detached and its timer invalidated.
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        self.ivars().status_item.setMenu(Some(&menu));
        *self.ivars().shown.borrow_mut() = Some(snapshot);
    }

    fn stop(&self) {
        if let Some(menu) = self.ivars().status_item.menu(self.mtm()) {
            menu.cancelTracking();
        }
        let app = NSApplication::sharedApplication(self.mtm());
        app.stop(None);
        // A timer can stop NSApplication while it waits for the next event.
        // Wake that wait so `run` returns and its retained objects are cleaned
        // up, without using `terminate:` or exiting the owning process.
        if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(),
            0.0,
            0,
            None,
            0,
            0,
            0,
        ) {
            app.postEvent_atStart(&event, true);
        }
    }
}

fn build_menu(target: &MenuTarget, entries: &[Entry]) -> Retained<NSMenu> {
    let menu = NSMenu::new(target.mtm());
    // AppKit's automatic validation would otherwise re-enable explicitly
    // disabled items or disable submenu parents with no action of their own.
    menu.setAutoenablesItems(false);
    for entry in entries {
        let item = match entry {
            Entry::Separator => NSMenuItem::separatorItem(target.mtm()),
            Entry::Label(label) => {
                let item = new_item(target.mtm(), label, false);
                item.setEnabled(false);
                item
            }
            Entry::Action { label, id, enabled } => {
                let item = new_item(target.mtm(), label, true);
                // SAFETY: The fixed selector is implemented by MenuTarget with
                // the NSMenuItem argument AppKit sends. The immutable action
                // ID is an NSString retained by this item's representedObject.
                // The target remains retained throughout the native run loop.
                unsafe {
                    item.setTarget(Some(target));
                    item.setRepresentedObject(Some(&NSString::from_str(id)));
                }
                item.setEnabled(*enabled);
                if id == QUIT {
                    item.setKeyEquivalent(ns_string!("q"));
                }
                item
            }
            Entry::Submenu {
                label,
                mark,
                entries,
            } => {
                let item = new_item(target.mtm(), &super::marked(label, *mark), false);
                item.setSubmenu(Some(&build_menu(target, entries)));
                item.setEnabled(true);
                item
            }
        };
        menu.addItem(&item);
    }
    menu
}

fn new_item(mtm: MainThreadMarker, label: &str, actionable: bool) -> Retained<NSMenuItem> {
    // SAFETY: The only selector is our MenuTarget method, set on action items
    // immediately after creation. Other items have no action.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(label),
            actionable.then(|| sel!(performMenuAction:)),
            ns_string!(""),
        )
    }
}

pub(super) fn run(
    initial: Menu,
    updates: Receiver<Menu>,
    actions: Sender<String>,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or("The macOS menu bar must be started on the process's main thread.")?;
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(mtm);
        // An LSUIElement app already starts as Accessory. AppKit may refuse
        // setting that same policy again, even though it is ready to run.
        if app.activationPolicy() != NSApplicationActivationPolicy::Accessory
            && !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory)
            && app.activationPolicy() != NSApplicationActivationPolicy::Accessory
        {
            return Err(format!(
                "macOS could not make devstack a menu bar application (activation policy: {:?}).",
                app.activationPolicy()
            ));
        }
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(28.0);
        let Some(button) = status_item.button(mtm) else {
            status_bar.removeStatusItem(&status_item);
            return Err("macOS could not create the devstack menu bar item.".into());
        };
        // The dashboard favicon's three strokes and dot, with no background.
        // A vector PDF stays sharp at every display scale; template rendering
        // follows macOS's light/dark menu bar and selected-item appearance.
        let icon_data = NSData::with_bytes(include_bytes!("assets/devstack-template.pdf"));
        if let Some(image) = NSImage::initWithData(NSImage::alloc(), &icon_data) {
            image.setSize(NSSize::new(20.0, 18.0));
            image.setTemplate(true);
            image.setAccessibilityDescription(Some(ns_string!("devstack")));
            button.setImage(Some(&image));
        } else {
            status_item.setLength(76.0);
            button.setTitle(ns_string!("devstack"));
        }
        button.setToolTip(Some(ns_string!("devstack")));
        status_item.setVisible(true);
        let target = MenuTarget::new(mtm, status_item.clone(), updates, actions);
        target.apply(initial);
        // SAFETY: `pollUpdates:` is implemented above with NSTimer's expected
        // signature. The retained target and timer live until after `run`.
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                0.2,
                &target,
                sel!(pollUpdates:),
                None,
                true,
            )
        };
        // SAFETY: The timer's callbacks and target are main-thread-only, and
        // this is the main run loop. Common modes keep callbacks responsive
        // while AppKit is tracking a menu.
        unsafe {
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        app.run();
        timer.invalidate();
        if let Some(menu) = status_item.menu(mtm) {
            menu.setDelegate(None);
        }
        status_item.setMenu(None);
        status_bar.removeStatusItem(&status_item);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_worker_thread_before_touching_appkit() {
        let result = std::thread::spawn(|| {
            let (_updates, receiver) = std::sync::mpsc::channel();
            let (sender, _actions) = std::sync::mpsc::channel();
            run(
                Menu {
                    tooltip: "devstack".into(),
                    ..Menu::default()
                },
                receiver,
                sender,
            )
        })
        .join()
        .unwrap();
        assert!(result.unwrap_err().contains("main thread"));
    }
}
