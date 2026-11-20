use std::cell::Cell;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use gdk_sys::GdkEventButton;
use glib_sys::{gboolean, gpointer};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk_sys::GtkStatusIcon;

use everything_core::config::Config;
use everything_core::ipc::Header;
use everything_core::model::QueryOptions;
use everything_core::sdk::LocalEngine;
const ICON_16: &[u8] = include_bytes!("../assets/icon-16.png");
const ICON_24: &[u8] = include_bytes!("../assets/icon-24.png");
const ICON_32: &[u8] = include_bytes!("../assets/icon-32.png");
const ICON_48: &[u8] = include_bytes!("../assets/icon-48.png");
const ICON_64: &[u8] = include_bytes!("../assets/icon-64.png");
const ICON_128: &[u8] = include_bytes!("../assets/icon-128.png");
const ICON_256: &[u8] = include_bytes!("../assets/icon-256.png");

fn app_icon(bytes: &'static [u8]) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    gtk::gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(bytes)).ok()
}

fn app_icons() -> Vec<gtk::gdk_pixbuf::Pixbuf> {
    [
        ICON_16, ICON_24, ICON_32, ICON_48, ICON_64, ICON_128, ICON_256,
    ]
    .into_iter()
    .filter_map(app_icon)
    .collect()
}

fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for &b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn open_target(path: &str) {
    let _ = gio::AppInfo::launch_default_for_uri(&file_uri(path), None::<&gio::AppLaunchContext>);
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
}

/// Open the containing folder and select the item via the standard
/// org.freedesktop.FileManager1 interface (Nautilus, Dolphin, Nemo, Caja,
/// Thunar 4.18+ etc. all implement it).
fn dbus_show_items(path: &str) -> bool {
    use gtk::glib::variant::ToVariant;
    let conn = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let args = (vec![file_uri(path)], "").to_variant();
    conn.call_sync(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        Some(&args),
        Some(gtk::glib::VariantTy::UNIT),
        gio::DBusCallFlags::NONE,
        3000,
        gio::Cancellable::NONE,
    )
    .is_ok()
}

/// Open the parent directory and highlight the selected file/folder.
fn open_parent_and_select(path: &str) {
    if dbus_show_items(path) {
        return;
    }
    for bin in ["nautilus", "dolphin", "nemo", "caja"] {
        if command_exists(bin) {
            let _ = std::process::Command::new(bin)
                .arg("--select")
                .arg(path)
                .spawn();
            return;
        }
    }
    // Last resort: just open the directory.
    if let Some(parent) = std::path::Path::new(path).parent() {
        open_target(&parent.to_string_lossy());
    }
}

fn copy_path(path: &str) {
    if let Some(display) = gtk::gdk::Display::default() {
        if let Some(clipboard) = gtk::Clipboard::default(&display) {
            clipboard.set_text(path);
        }
    }
}

struct TrayData {
    app: gtk::Application,
    window: gtk::ApplicationWindow,
    menu: gtk::Menu,
    last_click: Mutex<(u32, f64, f64)>,
    _hold: gio::ApplicationHoldGuard,
}

unsafe extern "C" fn on_status_icon_button_press(
    _icon: *mut GtkStatusIcon,
    event: *mut GdkEventButton,
    data: gpointer,
) -> gboolean {
    let data = unsafe { &*(data as *const TrayData) };
    let ev = unsafe { &*event };
    if ev.button == 1 && ev.type_ == gdk_sys::GDK_BUTTON_PRESS {
        let mut last = data.last_click.lock().unwrap();
        let now = ev.time;
        let is_double = now.wrapping_sub(last.0) < 400
            && (ev.x_root - last.1).abs() < 8.0
            && (ev.y_root - last.2).abs() < 8.0;
        *last = (now, ev.x_root, ev.y_root);
        drop(last);
        if is_double {
            data.window.show_all();
            data.window.present();
        }
    } else if ev.button == 3 && ev.type_ == gdk_sys::GDK_BUTTON_PRESS {
        data.menu.popup_at_pointer(None);
    }
    1
}

struct ResultMsg {
    header: Option<Header>,
    slots: Vec<u32>,
    root_generation: u64,
    error: Option<String>,
}

struct WindowRef(gtk::ApplicationWindow);
unsafe impl Send for WindowRef {}
unsafe impl Sync for WindowRef {}

struct EntryRef(gtk::Entry);
unsafe impl Send for EntryRef {}
unsafe impl Sync for EntryRef {}

static WINDOW: OnceLock<WindowRef> = OnceLock::new();
static ENTRY: OnceLock<EntryRef> = OnceLock::new();
static QUERY_TX: OnceLock<std::sync::mpsc::Sender<String>> = OnceLock::new();
static LAST_QUERY: OnceLock<Mutex<String>> = OnceLock::new();

fn gui_socket_path(config: &Config) -> PathBuf {
    let mut os = config.socket_path.clone().into_os_string();
    os.push(".gui");
    PathBuf::from(os)
}

fn notify_existing_gui(path: &PathBuf, exe: Option<&std::path::Path>) -> bool {
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            let msg = match exe {
                Some(p) => format!("SHOW {}\n", p.display()),
                None => "SHOW\n".to_string(),
            };
            let _ = stream.write_all(msg.as_bytes());
            let _ = stream.flush();
            true
        }
        Err(_) => false,
    }
}

fn spawn_gui_socket_listener(path: PathBuf, tx: glib::Sender<String>) {
    std::thread::Builder::new()
        .name("gui-single-instance".into())
        .spawn(move || {
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let listener = match UnixListener::bind(&path) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("cannot create single-instance socket: {e}");
                    return;
                }
            };
            for stream in listener.incoming() {
                match stream {
                    Ok(mut s) => {
                        let mut buf = [0u8; 1024];
                        let n = s.read(&mut buf).unwrap_or(0);
                        let text = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                        let exe = text
                            .strip_prefix("SHOW ")
                            .map(|p| p.trim().to_string())
                            .unwrap_or_default();
                        let _ = tx.send(exe);
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn single-instance listener");
}

fn present_existing_window() {
    if let Some(WindowRef(w)) = WINDOW.get() {
        w.show_all();
        w.present();
        if let Some(EntryRef(e)) = ENTRY.get() {
            e.grab_focus();
        }
    }
}

fn env_set(key: &str) -> bool {
    std::env::var_os(key)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Scan /proc for a running input-method framework (no external tools).
fn detect_im() -> Option<&'static str> {
    let mut fcitx = false;
    let mut ibus = false;
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
                match comm.trim() {
                    "fcitx5" | "fcitx" => fcitx = true,
                    "ibus-daemon" => ibus = true,
                    _ => {}
                }
            }
            if fcitx {
                break;
            }
        }
    }
    if fcitx {
        Some("fcitx")
    } else if ibus {
        Some("ibus")
    } else {
        None
    }
}

fn apply_im_env(fw: &str) {
    unsafe {
        std::env::set_var("GTK_IM_MODULE", fw);
        if !env_set("XMODIFIERS") {
            std::env::set_var("XMODIFIERS", format!("@im={fw}"));
        }
        if !env_set("QT_IM_MODULE") {
            std::env::set_var("QT_IM_MODULE", fw);
        }
    }
}

/// Best-effort pre-init: fill IM env if the launcher dropped it.
fn ensure_ime_env() {
    if env_set("GTK_IM_MODULE") {
        return;
    }
    if let Some(fw) = detect_im() {
        apply_im_env(fw);
    }
}

/// After GTK is initialized, verify that a real IM module (not the built-in
/// "simple" context) was loaded. If not, set the env and re-exec once.
fn maybe_reexec_for_im() {
    if env_set("EVERYTHING_IM_REEXEC") {
        return;
    }
    let ctx = gtk::IMMulticontext::new();
    use gtk::prelude::*;
    let id = ctx.context_id().map(|s| s.to_string()).unwrap_or_default();
    let simple = id.is_empty() || id.contains("simple") || id.contains("none");
    if !simple {
        return;
    }
    let Some(fw) = detect_im() else {
        return;
    };
    apply_im_env(fw);
    unsafe {
        std::env::set_var("EVERYTHING_IM_REEXEC", "1");
    }
    if let Ok(exe) = std::env::current_exe() {
        use std::os::unix::process::CommandExt;
        let _ = std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .exec();
    }
}

fn main() -> anyhow::Result<()> {
    ensure_ime_env();
    // Initialize GTK early so we can verify whether an IM module actually loaded.
    let gtk_ready = gtk::init().is_ok();
    if gtk_ready {
        maybe_reexec_for_im();
        let icons = app_icons();
        if !icons.is_empty() {
            gtk::Window::set_default_icon_list(&icons);
        }
    }
    let config = Config::load()?;
    let gui_socket = gui_socket_path(&config);
    let self_exe = std::env::current_exe().ok();

    // A second launch asks the running instance to raise its window and to
    // adopt this binary's location as the default root.
    if notify_existing_gui(&gui_socket, self_exe.as_deref()) {
        return Ok(());
    }

    let app = gtk::Application::builder()
        .application_id("com.local.everything-linux")
        .build();
    let local: Arc<OnceLock<Arc<LocalEngine>>> = Arc::new(OnceLock::new());
    let pending_root: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    #[allow(deprecated)]
    let (show_tx, show_rx) = glib::MainContext::channel(glib::Priority::DEFAULT);
    spawn_gui_socket_listener(gui_socket.clone(), show_tx);
    {
        let cfg = config.clone();
        let local_ui = local.clone();
        let pending = pending_root.clone();
        show_rx.attach(None, move |exe: String| {
            if !exe.is_empty() && cfg.auto_root {
                match local_ui.get().cloned() {
                    Some(engine) => {
                        std::thread::spawn(move || {
                            if engine
                                .set_auto_root(std::path::Path::new(&exe))
                                .unwrap_or(false)
                            {
                                let _ = engine.save_config();
                            }
                        });
                    }
                    None => {
                        *pending.lock().unwrap() = Some(exe);
                    }
                }
            }
            present_existing_window();
            glib::ControlFlow::Continue
        });
    }

    let local_app = local.clone();
    let pending_app = pending_root.clone();
    app.connect_activate(move |app| {
        if WINDOW.get().is_none() {
            build_ui(app, config.clone(), local_app.clone(), pending_app.clone());
        } else {
            present_existing_window();
        }
    });
    app.run();
    let _ = std::fs::remove_file(gui_socket);
    Ok(())
}

fn build_ui(
    app: &gtk::Application,
    config: Config,
    local: Arc<OnceLock<Arc<LocalEngine>>>,
    pending_root: Arc<Mutex<Option<String>>>,
) {
    let hold = app.hold();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Everything (Linux)")
        .default_width(900)
        .default_height(600)
        .build();
    let _ = WINDOW.set(WindowRef(window.clone()));

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);

    let entry = gtk::Entry::new();
    let _ = ENTRY.set(EntryRef(entry.clone()));
    entry.set_placeholder_text(Some(
        "Search file names...  (e.g. *.rs  size:>1mb  ext:rs|md)",
    ));
    entry.set_hexpand(true);

    let store = gtk::ListStore::new(&[u32::static_type(), u64::static_type()]);
    let tree = gtk::TreeView::with_model(&store);
    let column = gtk::TreeViewColumn::new();
    column.set_title("Path");
    let renderer = gtk::CellRendererText::new();
    gtk::prelude::CellLayoutExt::pack_start(&column, &renderer, true);
    {
        let local_cell = local.clone();
        gtk::prelude::TreeViewColumnExt::set_cell_data_func(
            &column,
            &renderer,
            Some(Box::new(move |_col, cell, model, iter| {
                let slot: u32 = model.value(iter, 0).get().unwrap_or(u32::MAX);
                let row_epoch: u64 = model.value(iter, 1).get().unwrap_or(0);
                let text = local_cell
                    .get()
                    .filter(|engine| engine.root_generation() == row_epoch)
                    .and_then(|engine| engine.path(slot))
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                cell.set_property("text", text);
            })),
        );
    }
    tree.append_column(&column);
    tree.set_headers_visible(false);

    let scroll = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    scroll.set_vexpand(true);
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scroll.add(&tree);

    let status = gtk::Label::new(Some("Ready"));
    status.set_xalign(0.0);

    vbox.pack_start(&entry, false, false, 0);
    vbox.pack_start(&scroll, true, true, 0);
    vbox.pack_start(&status, false, false, 0);
    window.add(&vbox);

    let (query_tx, query_rx) = std::sync::mpsc::channel::<String>();
    let _ = QUERY_TX.set(query_tx.clone());
    let _ = LAST_QUERY.set(Mutex::new(String::new()));
    #[allow(deprecated)]
    let (result_tx, result_rx) = glib::MainContext::channel(glib::Priority::DEFAULT);

    let config_worker = config.clone();
    let local_worker = local.clone();
    let pending_worker = pending_root.clone();
    std::thread::Builder::new()
        .name("gui-query".into())
        .spawn(move || {
            let mut cfg = config_worker;
            let exe = std::env::current_exe().ok();
            let roots_changed = cfg.apply_exe_root(exe.as_deref());
            if roots_changed {
                let _ = cfg.save();
            }
            let engine = match LocalEngine::open(cfg, !roots_changed) {
                Ok(engine) => Arc::new(engine),
                Err(e) => {
                    let _ = result_tx.send(ResultMsg {
                        header: None,
                        slots: Vec::new(),
                        root_generation: 0,
                        error: Some(e.to_string()),
                    });
                    return;
                }
            };
            let _ = local_worker.set(engine.clone());
            if let Some(root_exe) = pending_worker.lock().unwrap().take() {
                if engine
                    .set_auto_root(std::path::Path::new(&root_exe))
                    .unwrap_or(false)
                {
                    let _ = engine.save_config();
                }
            }
            while let Ok(mut text) = query_rx.recv() {
                std::thread::sleep(std::time::Duration::from_millis(80));
                while let Ok(newer) = query_rx.try_recv() {
                    text = newer;
                }
                let options = QueryOptions {
                    limit: 5000,
                    ..Default::default()
                };
                let result = engine.search_slots(&text, options);
                let root_generation = engine.root_generation();
                let msg = match result {
                    Ok(result) => ResultMsg {
                        header: Some(Header {
                            ok: true,
                            count: result.slots.len(),
                            total: result.total_matched,
                            elapsed_ms: result.elapsed_ms,
                            generation: result.generation,
                            truncated: result.truncated,
                            message: String::new(),
                        }),
                        slots: result.slots,
                        root_generation,
                        error: None,
                    },
                    Err(e) => ResultMsg {
                        header: None,
                        slots: Vec::new(),
                        root_generation,
                        error: Some(e.to_string()),
                    },
                };
                let _ = result_tx.send(msg);
            }
        })
        .expect("spawn query worker");

    {
        let store = store.clone();
        let status = status.clone();
        result_rx.attach(None, move |msg: ResultMsg| {
            store.clear();
            for slot in &msg.slots {
                store.set(&store.append(), &[(0, slot), (1, &msg.root_generation)]);
            }
            let label = match (msg.header, msg.error) {
                (Some(h), _) if h.ok => format!(
                    "{} results ({} total)  {:.1} ms  gen {}",
                    h.count, h.total, h.elapsed_ms, h.generation
                ),
                (_, Some(e)) => e,
                _ => "No results".into(),
            };
            status.set_label(&label);
            glib::ControlFlow::Continue
        });
    }

    {
        let tx = query_tx.clone();
        entry.connect_changed(move |e| {
            let text = e.text().to_string();
            if let Some(last) = LAST_QUERY.get() {
                *last.lock().unwrap() = text.clone();
            }
            let _ = tx.send(text);
        });
    }
    entry.connect_activate(|_| {});

    // A root switch invalidates all slots. Refresh the current query when the
    // root epoch changes; ordinary inotify increments leave rows/menu intact.
    {
        let local_epoch = local.clone();
        let tx = query_tx.clone();
        let last_epoch = Cell::new(0u64);
        glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
            if let Some(engine) = local_epoch.get() {
                let epoch = engine.root_generation();
                if last_epoch.get() == 0 {
                    last_epoch.set(epoch);
                } else if epoch != last_epoch.get() {
                    last_epoch.set(epoch);
                    if let Some(last) = LAST_QUERY.get() {
                        let text = last.lock().unwrap().clone();
                        if !text.is_empty() {
                            let _ = tx.send(text);
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let model = tree.model().unwrap();
        let local_rows = local.clone();
        tree.connect_row_activated(move |_tree, path, _col| {
            if let Some(iter) = model.iter(path) {
                let slot: u32 = model.value(&iter, 0).get().unwrap_or(u32::MAX);
                let row_gen: u64 = model.value(&iter, 1).get().unwrap_or(0);
                if let Some(p) = local_rows
                    .get()
                    .filter(|engine| engine.root_generation() == row_gen)
                    .and_then(|engine| engine.path(slot))
                {
                    open_target(&p.to_string_lossy());
                }
            }
        });
    }

    // Right-click context menu on a result row: open / open path / copy path.
    {
        let model = tree.model().unwrap();
        let local_menu = local.clone();
        tree.connect_button_press_event(move |tree, event| {
            if event.button() != 3 {
                return glib::Propagation::Proceed;
            }
            let (x, y) = event.position();
            if let Some((Some(path), _, _, _)) = tree.path_at_pos(x as i32, y as i32) {
                tree.selection().select_path(&path);
                if let Some(iter) = model.iter(&path) {
                    let slot: u32 = model.value(&iter, 0).get().unwrap_or(u32::MAX);
                    let row_gen: u64 = model.value(&iter, 1).get().unwrap_or(0);
                    let value = local_menu
                        .get()
                        .filter(|engine| engine.root_generation() == row_gen)
                        .and_then(|engine| engine.path(slot))
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if !value.is_empty() {
                        let menu = gtk::Menu::new();
                        let open = gtk::MenuItem::with_label("打开");
                        let open_path = gtk::MenuItem::with_label("打开路径");
                        let copy = gtk::MenuItem::with_label("复制完整路径和文件名");
                        let v1 = value.clone();
                        open.connect_activate(move |_| open_target(&v1));
                        let v2 = value.clone();
                        open_path.connect_activate(move |_| open_parent_and_select(&v2));
                        let v3 = value.clone();
                        copy.connect_activate(move |_| copy_path(&v3));
                        menu.append(&open);
                        menu.append(&open_path);
                        menu.append(&copy);
                        menu.show_all();
                        menu.popup_at_pointer(None);
                        return glib::Propagation::Stop;
                    }
                }
            }
            glib::Propagation::Proceed
        });
    }

    let tx_esc = query_tx.clone();
    window.connect_key_press_event(move |w, key| {
        if key.keyval() == gtk::gdk::keys::constants::Escape {
            w.close();
            let _ = &tx_esc;
        }
        glib::Propagation::Proceed
    });

    // Close button: minimize to tray instead of quitting the GUI.
    window.connect_delete_event(|w, _| {
        w.hide();
        glib::Propagation::Stop
    });

    // Right-click menu on the tray icon.
    let menu = gtk::Menu::new();
    let open_item = gtk::MenuItem::with_label("打开界面");
    let quit_item = gtk::MenuItem::with_label("退出");
    menu.append(&open_item);
    menu.append(&gtk::SeparatorMenuItem::new());
    menu.append(&quit_item);
    menu.show_all();

    {
        let w = window.clone();
        open_item.connect_activate(move |_| {
            w.show_all();
            w.present();
        });
    }
    {
        let a = app.clone();
        quit_item.connect_activate(move |_| a.quit());
    }

    let tray = Box::leak(Box::new(TrayData {
        app: app.clone(),
        window: window.clone(),
        menu,
        last_click: Mutex::new((0, 0.0, 0.0)),
        _hold: hold,
    }));

    let icon = match app_icon(ICON_48) {
        Some(pixbuf) => unsafe {
            gtk_sys::gtk_status_icon_new_from_pixbuf(
                gtk::glib::translate::ToGlibPtr::to_glib_none(&pixbuf).0,
            )
        },
        None => {
            let theme = gtk::IconTheme::default().expect("default icon theme");
            let chosen = ["system-search", "edit-find", "gtk-find"]
                .into_iter()
                .find(|n| theme.has_icon(n))
                .unwrap_or("applications-system");
            let icon_name = std::ffi::CString::new(chosen).unwrap();
            unsafe { gtk_sys::gtk_status_icon_new_from_icon_name(icon_name.as_ptr()) }
        }
    };
    if !icon.is_null() {
        install_tray_handlers(icon, tray);
    }

    window.set_focus_on_map(true);
    window.show_all();
    window.set_focus(Some(&entry));
    entry.grab_focus();
    {
        // Defer focus until the toplevel is mapped/active (double-click launch timing).
        let e = entry.clone();
        glib::idle_add_local_once(move || {
            e.grab_focus();
        });
    }
    {
        // When the window regains focus (alt-tab, tray restore), make sure the
        // search box owns the input method context.
        let e = entry.clone();
        window.connect_focus_in_event(move |_, _| {
            if !e.has_focus() {
                e.grab_focus();
            }
            glib::Propagation::Proceed
        });
    }
    let _ = query_tx.send(String::new());
}

fn install_tray_handlers(icon: *mut GtkStatusIcon, tray: &'static TrayData) {
    let tooltip = std::ffi::CString::new("Everything Linux - 双击打开，右键退出").unwrap();
    unsafe {
        gtk_sys::gtk_status_icon_set_tooltip_text(icon, tooltip.as_ptr());
        gtk_sys::gtk_status_icon_set_visible(icon, 1);
        let _handler = glib::signal::connect_raw(
            icon as *mut glib::gobject_ffi::GObject,
            b"button-press-event\0".as_ptr() as *const _,
            Some(std::mem::transmute::<
                unsafe extern "C" fn(*mut GtkStatusIcon, *mut GdkEventButton, gpointer) -> gboolean,
                unsafe extern "C" fn(),
            >(on_status_icon_button_press)),
            tray as *const TrayData as *mut std::ffi::c_void,
        );
    }
    let _ = &tray.app;
}

#[cfg(test)]
mod icon_tests {
    use super::*;

    #[test]
    fn embedded_window_and_tray_icons_decode() {
        assert_eq!(app_icons().len(), 7);
        assert!(app_icon(ICON_48).is_some());
    }
}
