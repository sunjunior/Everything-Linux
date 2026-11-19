use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use gdk_sys::GdkEventButton;
use glib_sys::{gboolean, gpointer};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk_sys::GtkStatusIcon;

use everything_core::config::Config;
use everything_core::ipc::{self, Header, Request, WireOptions};

const EMBEDDED_DAEMON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/everythingd"));

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
    let _ = gio::AppInfo::launch_default_for_uri(
        &file_uri(path),
        None::<&gio::AppLaunchContext>,
    );
}

fn open_parent(path: &str) {
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
    hold: gio::ApplicationHoldGuard,
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
    lines: Vec<String>,
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

fn main() -> anyhow::Result<()> {
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
    let (show_tx, show_rx) = glib::MainContext::channel(glib::Priority::DEFAULT);
    spawn_gui_socket_listener(gui_socket.clone(), show_tx);
    {
        let cfg = config.clone();
        show_rx.attach(None, move |exe: String| {
            if !exe.is_empty() && cfg.auto_root {
                let _ = ipc::set_auto_root(&cfg, std::path::Path::new(&exe));
            }
            present_existing_window();
            glib::ControlFlow::Continue
        });
    }

    app.connect_activate(move |app| {
        if WINDOW.get().is_none() {
            build_ui(app, config.clone());
        } else {
            present_existing_window();
        }
    });
    app.run();
    let _ = std::fs::remove_file(gui_socket);
    Ok(())
}

fn build_ui(app: &gtk::Application, config: Config) {
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
    entry.set_placeholder_text(Some("Search file names...  (e.g. *.rs  size:>1mb  ext:rs|md)"));
    entry.set_hexpand(true);

    let store = gtk::ListStore::new(&[String::static_type()]);
    let tree = gtk::TreeView::with_model(&store);
    let column = gtk::TreeViewColumn::new();
    column.set_title("Path");
    let renderer = gtk::CellRendererText::new();
    gtk::prelude::CellLayoutExt::pack_start(&column, &renderer, true);
    gtk::prelude::CellLayoutExt::add_attribute(&column, &renderer, "text", 0);
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
    let (result_tx, result_rx) = glib::MainContext::channel(glib::Priority::DEFAULT);

    let config_worker = config.clone();
    let gui_exe = std::env::current_exe().ok();
    std::thread::Builder::new()
        .name("gui-query".into())
        .spawn(move || {
            if let Err(e) = ipc::ensure_daemon(&config_worker, gui_exe.as_deref(), Some(EMBEDDED_DAEMON)) {
                let _ = result_tx.send(ResultMsg {
                    header: None,
                    lines: Vec::new(),
                    error: Some(e.to_string()),
                });
                return;
            }
            if let Some(exe) = gui_exe.as_deref() {
                let adopted = ipc::set_auto_root(&config_worker, exe).unwrap_or(false);
                if !adopted && config_worker.auto_root {
                    // Older/foreign daemon: restart it so the new default-root
                    // behaviour and this binary are in effect.
                    ipc::shutdown_daemon(&config_worker);
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let _ = std::fs::remove_file(&config_worker.socket_path);
                    if let Err(e) = ipc::ensure_daemon(&config_worker, gui_exe.as_deref(), Some(EMBEDDED_DAEMON)) {
                        let _ = result_tx.send(ResultMsg { header: None, lines: Vec::new(), error: Some(e.to_string()) });
                        return;
                    }
                    let _ = ipc::set_auto_root(&config_worker, exe);
                }
            }
            while let Ok(mut text) = query_rx.recv() {
                std::thread::sleep(std::time::Duration::from_millis(80));
                while let Ok(newer) = query_rx.try_recv() {
                    text = newer;
                }
                let mut options = WireOptions::default();
                options.limit = 5000;
                let request = Request {
                    cmd: "query".into(),
                    text: text.clone(),
                    options,
                };
                let msg = match ipc::client_call(&config_worker.socket_path, &request) {
                    Ok((header, lines)) => ResultMsg {
                        header: Some(header),
                        lines,
                        error: None,
                    },
                    Err(e) => ResultMsg {
                        header: None,
                        lines: Vec::new(),
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
            for line in &msg.lines {
                store.set(&store.append(), &[(0, &line)]);
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
            let _ = tx.send(e.text().to_string());
        });
    }
    entry.connect_activate(|_| {});

    {
        let model = tree.model().unwrap();
        tree.connect_row_activated(move |_tree, path, _col| {
            if let Some(iter) = model.iter(path) {
                let value: String = model.value(&iter, 0).get().unwrap_or_default();
                open_target(&value);
            }
        });
    }

    // Right-click context menu on a result row: open / open path / copy path.
    {
        let model = tree.model().unwrap();
        tree.connect_button_press_event(move |tree, event| {
            if event.button() != 3 {
                return glib::Propagation::Proceed;
            }
            let (x, y) = event.position();
            if let Some((Some(path), _, _, _)) = tree.path_at_pos(x as i32, y as i32) {
                tree.selection().select_path(&path);
                if let Some(iter) = model.iter(&path) {
                    let value: String = model.value(&iter, 0).get().unwrap_or_default();
                    if !value.is_empty() {
                        let menu = gtk::Menu::new();
                        let open = gtk::MenuItem::with_label("打开");
                        let open_path = gtk::MenuItem::with_label("打开路径");
                        let copy = gtk::MenuItem::with_label("复制完整路径和文件名");
                        let v1 = value.clone();
                        open.connect_activate(move |_| open_target(&v1));
                        let v2 = value.clone();
                        open_path.connect_activate(move |_| open_parent(&v2));
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
        hold,
    }));

    let theme = gtk::IconTheme::default().expect("default icon theme");
    let chosen = ["system-search", "edit-find", "gtk-find"]
        .into_iter()
        .find(|n| theme.has_icon(n))
        .unwrap_or("applications-system");
    let icon_name = std::ffi::CString::new(chosen).unwrap();
    let icon = unsafe { gtk_sys::gtk_status_icon_new_from_icon_name(icon_name.as_ptr()) };
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
