//! Linux: a StatusNotifierItem (`org.kde.StatusNotifierItem`) with its menu
//! as `com.canonical.dbusmenu`, both on the session bus in pure Rust, so
//! the binary stays static (ADR-0060). KDE, Xfce, Cinnamon, and GNOME with
//! the AppIndicator extension show it. Without a StatusNotifierWatcher on
//! the bus (GNOME without it) the item waits and registers when one comes.

use super::{Entry, Menu, QUIT, Tone};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_stream::StreamExt;
use zbus::object_server::SignalEmitter;
use zbus::{OwnedObjectPath, OwnedVariant, Type, Variant, interface};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// The sizes the icon is offered in; the host picks.
const SIZES: [u32; 5] = [16, 22, 24, 32, 48];

pub fn unavailable() -> Option<String> {
    let address = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|v| !v.is_empty());
    let socket = std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|d| !d.is_empty() && Path::new(&d).join("bus").exists());
    (!address && !socket).then(|| "no desktop session (no D-Bus session bus)".into())
}

/// What both objects read: the menu shown, and its layout's revision.
struct Shared {
    menu: Menu,
    revision: u32,
}

/// The icon, in ARGB32 with the bytes in network order, as the spec wants.
type Pixmap = (i32, i32, Vec<u8>);

fn pixmaps(tone: Tone) -> Vec<Pixmap> {
    SIZES
        .iter()
        .map(|&size| {
            let rgba = super::rgba(size, tone);
            let argb = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[3], p[0], p[1], p[2]])
                .collect();
            (size as i32, size as i32, argb)
        })
        .collect()
}

struct Item {
    shared: Arc<Mutex<Shared>>,
    actions: Mutex<Sender<String>>,
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "devstack"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "devstack"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn window_id(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        pixmaps(self.shared.lock().unwrap().menu.tone)
    }
    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }
    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }
    /// Icon name, icon, title, text.
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        let tip = self.shared.lock().unwrap().menu.tooltip.clone();
        (String::new(), Vec::new(), "devstack".into(), tip)
    }
    /// A click shows the menu; `Activate` is not needed then.
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(MENU_PATH).expect("a valid path")
    }

    /// A host that activates anyway (KDE's left click): the dashboard.
    fn activate(&self, _x: i32, _y: i32) {
        let _ = self.actions.lock().unwrap().send("dashboard".into());
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {}
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// A node of the menu's layout: `(ia{sv}av)`.
#[derive(Debug, Serialize, Type)]
struct Layout {
    id: i32,
    props: HashMap<String, OwnedVariant>,
    children: Vec<OwnedVariant>,
}

/// A menu item as the layout has it, with the action it sends.
struct Node {
    id: i32,
    props: HashMap<String, OwnedVariant>,
    action: Option<String>,
    children: Vec<Node>,
}

fn owned<'a>(v: impl Into<Variant<'a>>) -> OwnedVariant {
    OwnedVariant::try_from(v.into()).expect("no file descriptors in a menu")
}

/// `_` marks an access key in a dbusmenu label.
fn label(text: &str) -> OwnedVariant {
    owned(text.replace('_', "__"))
}

/// The menu as a tree of nodes, numbered depth first from 1 (the root is
/// 0), the same numbers for the same menu.
fn tree(entries: &[Entry], next: &mut i32) -> Vec<Node> {
    entries
        .iter()
        .map(|e| {
            *next += 1;
            let id = *next;
            let mut props = HashMap::new();
            let mut action = None;
            let mut children = Vec::new();
            match e {
                Entry::Separator => {
                    props.insert("type".into(), owned("separator"));
                }
                Entry::Label(text) => {
                    props.insert("label".into(), label(text));
                    props.insert("enabled".into(), owned(false));
                }
                Entry::Action {
                    label: text,
                    id: action_id,
                    enabled,
                } => {
                    props.insert("label".into(), label(text));
                    props.insert("enabled".into(), owned(*enabled));
                    action = Some(action_id.clone());
                }
                Entry::Submenu {
                    label: text,
                    entries,
                } => {
                    props.insert("label".into(), label(text));
                    props.insert("children-display".into(), owned("submenu"));
                    children = tree(entries, next);
                }
            }
            Node {
                id,
                props,
                action,
                children,
            }
        })
        .collect()
}

fn root(menu: &Menu) -> Node {
    let mut props = HashMap::new();
    props.insert("children-display".into(), owned("submenu"));
    Node {
        id: 0,
        props,
        action: None,
        children: tree(&menu.entries, &mut 0),
    }
}

fn find(node: &Node, id: i32) -> Option<&Node> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter().find_map(|c| find(c, id))
}

/// `depth`: how many levels of children; `-1` all.
fn layout(node: &Node, depth: i32) -> Layout {
    let children = if depth == 0 {
        Vec::new()
    } else {
        node.children
            .iter()
            .map(|c| {
                let l = layout(c, depth - 1);
                owned(Variant::from(zbus::Structure::from((
                    l.id, l.props, l.children,
                ))))
            })
            .collect()
    };
    Layout {
        id: node.id,
        props: node.props.clone(),
        children,
    }
}

struct DbusMenu {
    shared: Arc<Mutex<Shared>>,
    actions: Mutex<Sender<String>>,
    stop: Arc<tokio::sync::Notify>,
}

impl DbusMenu {
    fn clicked(&self, id: i32) {
        let action = {
            let s = self.shared.lock().unwrap();
            find(&root(&s.menu), id).and_then(|n| n.action.clone())
        };
        let Some(action) = action else {
            return;
        };
        let quit = action == QUIT;
        let sent = self.actions.lock().unwrap().send(action).is_ok();
        if quit || !sent {
            self.stop.notify_one();
        }
    }
}

#[interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(out_args("revision", "layout"))]
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        let s = self.shared.lock().unwrap();
        let root = root(&s.menu);
        let node = find(&root, parent_id)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no item {parent_id}")))?;
        Ok((s.revision, layout(node, recursion_depth)))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> Vec<(i32, HashMap<String, OwnedVariant>)> {
        let s = self.shared.lock().unwrap();
        let root = root(&s.menu);
        ids.into_iter()
            .filter_map(|id| find(&root, id).map(|n| (id, n.props.clone())))
            .collect()
    }

    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<OwnedVariant> {
        let s = self.shared.lock().unwrap();
        find(&root(&s.menu), id)
            .and_then(|n| n.props.get(name).cloned())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no {name} on item {id}")))
    }

    fn event(&self, id: i32, event_id: &str, _data: OwnedVariant, _timestamp: u32) {
        if event_id == "clicked" {
            self.clicked(id);
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedVariant, u32)>) -> Vec<i32> {
        for (id, event_id, _, _) in &events {
            if event_id == "clicked" {
                self.clicked(*id);
            }
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[zbus(out_args("updates_needed", "id_errors"))]
    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

pub fn run(initial: Menu, updates: Receiver<Menu>, actions: Sender<String>) -> Result<(), String> {
    if let Some(why) = unavailable() {
        return Err(why);
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(serve(initial, updates, actions))
}

async fn serve(
    initial: Menu,
    updates: Receiver<Menu>,
    actions: Sender<String>,
) -> Result<(), String> {
    let shared = Arc::new(Mutex::new(Shared {
        menu: initial,
        revision: 1,
    }));
    let stop = Arc::new(tokio::sync::Notify::new());
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let conn = zbus::connection::Builder::session()
        .name(name.as_str())
        .serve_at(
            ITEM_PATH,
            Item {
                shared: shared.clone(),
                actions: Mutex::new(actions.clone()),
            },
        )
        .serve_at(
            MENU_PATH,
            DbusMenu {
                shared: shared.clone(),
                actions: Mutex::new(actions),
                stop: stop.clone(),
            },
        )
        .build()
        .await
        .map_err(|e| format!("no desktop session (D-Bus session bus: {e})"))?;
    register(&conn, &name).await;
    let watcher = tokio::spawn(follow_watcher(conn.clone(), name.clone()));

    let item = conn
        .object_server()
        .interface::<_, Item>(ITEM_PATH)
        .await
        .map_err(|e| e.to_string())?;
    let menu = conn
        .object_server()
        .interface::<_, DbusMenu>(MENU_PATH)
        .await
        .map_err(|e| e.to_string())?;
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            _ = stop.notified() => break,
            _ = tick.tick() => {}
        }
        let mut next = None;
        loop {
            match updates.try_recv() {
                Ok(m) => next = Some(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    watcher.abort();
                    return Ok(());
                }
            }
        }
        let Some(next) = next else {
            continue;
        };
        let (icon, tip, layout, revision) = {
            let mut s = shared.lock().unwrap();
            if s.menu == next {
                continue;
            }
            let icon = s.menu.tone != next.tone;
            let tip = s.menu.tooltip != next.tooltip;
            let layout = s.menu.entries != next.entries;
            if layout {
                s.revision += 1;
            }
            s.menu = next;
            (icon, tip, layout, s.revision)
        };
        let emitter = item.signal_emitter();
        if icon {
            let _ = Item::new_icon(emitter).await;
        }
        if tip {
            let _ = Item::new_tool_tip(emitter).await;
        }
        if layout {
            let _ = DbusMenu::layout_updated(menu.signal_emitter(), revision, 0).await;
        }
    }
    watcher.abort();
    Ok(())
}

/// Tell the watcher about the item; quiet when there is none (yet).
async fn register(conn: &zbus::Connection, name: &str) {
    let _ = conn
        .call_method(
            Some(WATCHER),
            "/StatusNotifierWatcher",
            Some(WATCHER),
            "RegisterStatusNotifierItem",
            &(name,),
        )
        .await;
}

/// Register again whenever a watcher appears: the desktop's shell restarted,
/// or the AppIndicator extension was switched on.
async fn follow_watcher(conn: zbus::Connection, name: String) {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else {
        return;
    };
    let Ok(mut changes) = dbus.receive_name_owner_changed().await else {
        return;
    };
    while let Some(change) = changes.next().await {
        let Ok(args) = change.args() else {
            continue;
        };
        if args.name().as_str() == WATCHER && args.new_owner().is_some() {
            register(&conn, &name).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        Menu {
            tone: Tone::Ok,
            tooltip: "devstack".into(),
            entries: vec![
                Entry::Label("devstack: 1 stack running".into()),
                Entry::Separator,
                Entry::Submenu {
                    label: "shop_app".into(),
                    entries: vec![Entry::Action {
                        label: "Stop".into(),
                        id: "stop:ab12".into(),
                        enabled: true,
                    }],
                },
                Entry::Action {
                    label: "Quit".into(),
                    id: QUIT.into(),
                    enabled: true,
                },
            ],
        }
    }

    #[test]
    fn items_are_numbered_depth_first_and_find_their_action() {
        let root = root(&menu());
        assert_eq!(
            find(&root, 4).and_then(|n| n.action.as_deref()),
            Some("stop:ab12")
        );
        assert_eq!(find(&root, 5).and_then(|n| n.action.as_deref()), Some(QUIT));
        assert!(find(&root, 1).unwrap().action.is_none());
        let sub = find(&root, 3).unwrap();
        assert_eq!(
            sub.props["label"],
            OwnedVariant::try_from(Variant::from("shop__app")).unwrap()
        );
        assert_eq!(layout(&root, -1).children.len(), 4);
        assert!(layout(&root, 0).children.is_empty());
    }

    #[test]
    fn the_layout_has_the_dbusmenu_signature() {
        assert_eq!(Layout::SIGNATURE.to_string(), "(ia{sv}av)");
        assert_eq!(<Vec<Pixmap>>::SIGNATURE.to_string(), "a(iiay)");
    }

    #[test]
    fn the_icon_is_argb_in_network_order() {
        let p = pixmaps(Tone::Attention);
        assert_eq!(p.len(), SIZES.len());
        let (w, h, bytes) = &p[0];
        assert_eq!((*w, *h, bytes.len()), (16, 16, 16 * 16 * 4));
        assert_eq!(bytes[0], 0, "a clear corner: alpha first");
    }
}
