# GUI 选型与分发构建

## GUI 选型：GTK 3

选择 **GTK 3（`gtk` crate 0.18）**，理由：

- 本机 `gtk+-3.0 = 3.24.52`、`webkit2gtk-4.1 = 2.54.1`，GTK3 是发行版最稳定的桌面库；
- Qt crate 未缓存，且 Qt 运行库体积大、许可更复杂；
- Tauri/WebView 会引入 webkit2gtk 这样的大型依赖，不符合"文件不能搞太大、复用系统库"的要求；
- GTK3 系统库几乎所有 x86-64 Linux 桌面都自带，GUI 二进制只有 **808 KB**（release，stripped）。

GUI 通过 Unix socket IPC 复用同一个 `everything_core` 查询引擎，不重复实现搜索逻辑。
功能：输入即时查询（80ms 防抖）、结果列表、状态栏（结果数/耗时/generation）、双击用系统默认程序打开文件、守护进程未启动时自动拉起。

## 静态可执行文件

`build-static.sh` 生成 `dist/`：

| 文件 | 链接方式 | 大小 | 说明 |
|---|---|---|---|
| `everything` | **static-pie** | 1.8 MB | CLI 客户端，任意 x86-64 Linux 可直接运行 |
| `everythingd` | **static-pie** | 4.7 MB | 守护进程，任意 x86-64 Linux 可直接运行 |
| `everything-gtk` | 动态链接系统 GTK3 | 808 KB | GUI，需要系统 `libgtk-3.so.0` 等 |

构建方式：
```bash
./build-static.sh
```

静态链接通过对最终二进制使用 `-C target-feature=+crt-static`（`cargo rustc`）实现；
不能对整个 workspace 设置 `RUSTFLAGS`，否则 proc-macro  crate 无法构建。

### 关于"一个静态可执行文件"的取舍

**CLI/daemon 已完全静态，可单文件部署。GUI 无法现实地做成静态**：GTK 依赖 glib/gdk/pango/cairo/X11/Wayland 以及图标主题、动态模块加载，静态链接会产生几十 MB 且脆弱的二进制。
因此采用"静态核心 + 复用系统 GTK 的小 GUI"方案，既满足可移植性又满足体积要求。没有 GTK3 的环境使用 CLI 即可。

## 使用 GUI

```bash
# 先确保守护进程已配置索引根（或由 GUI 自动拉起）
./dist/everything-gtk
```

查询框支持与 CLI 相同的语法（含 `size:`、`dm:`、`ext:` 等属性过滤）。

## 运行依赖

完整的依赖清单、glibc / GTK3 版本下限、传递依赖库列表与目标机自查脚本见 `DEPENDENCIES.md`。

## 任务栏图标（v0.2）

- 点击窗口右上角 **关闭按钮**：窗口隐藏到任务栏图标，**守护进程与索引继续常驻**，不退出程序。
- **双击任务栏图标**：重新打开并激活搜索窗口（左键双击判定：400ms 内、位移 < 8px）。
- **右键任务栏图标**：弹出菜单，含「打开界面」与「退出」；点击「退出」才真正关闭 GUI 程序。
- 实现：GTK3 `GtkStatusIcon`（通过 `gtk-sys` FFI，因为 gtk-rs 0.18 未包装该控件），图标从系统图标主题自动选择（`system-search` → `edit-find` → `gtk-find`）。

## 单实例运行（v0.3）

- 重复双击 / 多次运行 `everything-gtk` **不会**弹出"已在运行"提示，也**不会**启动第二个窗口或进程。
- 第二个进程通过专用 Unix socket（`$XDG_RUNTIME_DIR/everything-linux.sock.gui`，基于 daemon socket 路径加 `.gui` 后缀）向已运行的实例发送 `SHOW` 请求，随后以退出码 0 立即退出。
- 已运行实例收到请求后调用 `window.show_all()` + `window.present()`，把主界面切换到前台；窗口此前处于隐藏到托盘状态时也会恢复显示。
- 实现位置：`crates/everything-gtk/src/main.rs` 的 `gui_socket_path` / `notify_existing_gui` / `spawn_gui_socket_listener` / `present_existing_window`。
- 全局 `WINDOW` 缓存保证 GIO `activate` 回调与 socket 请求都复用同一个窗口；GIO Application 自身也注册了 application id，作为额外一层单实例保护。

## 默认索引根与程序移动（v0.4）

- 首次启动（没有配置文件）时，以**当前可执行文件所在目录**作为默认索引根，并写入配置 `roots` 与 `exe_path`。
- 再次启动（第二次及以后）直接沿用配置中的索引根，不重复提示、不改配置。
- 程序关闭后被移动到新路径，下次启动检测到 `exe_path` 与当前可执行文件不同，会**自动把默认索引根重设为新路径**并重建索引（跳过旧快照）。
- 如果用户手动编辑了 `roots`，或使用 `everything index add/remove`，配置会标记 `auto_root = false`，此后索引根**不再随程序移动而改变**。

## 一键运行：前后台的关系（v0.5）

**运行时仍是前后台两个进程**（GUI 负责界面，守护进程负责索引/inotify/IPC），但**分发上 GUI 单文件即可**：

- `everything-gtk` 在构建时把静态的 `everythingd` **内嵌进自身**（`build.rs` + `include_bytes!`），产物约 5.5 MB；
- 首次运行若本机没有 `everythingd`，GUI 会按内嵌字节的哈希把守护进程解压到
  `~/.cache/everything-linux/everythingd-<hash>`（自动加可执行权限），然后以
  `--root <GUI所在目录>` 启动它；
- 因此**把 `everything-gtk` 单独拷到任意 x86-64 Linux 机器的任意目录，直接双击即可使用**：
  默认索引根目录就是该 GUI 文件所在目录，索引库/配置/socket 使用标准 XDG 路径；
- 若同目录或 PATH 中已有独立 `everythingd`，优先使用它（便于自定义升级）；
- CLI `everything` 仍是独立静态文件（1.8 MB），不会内嵌守护进程；它也会在同目录/PATH 找不到守护进程时给出明确提示。

注意：GUI 单文件仍然**动态依赖系统 GTK3**（见 `DEPENDENCIES.md`）；守护进程是静态的，
内嵌后解压出来的也是静态文件，不依赖目标机器的 glibc 版本。

## 结果列表右键菜单（v0.6）

结果列表中右键某一行会弹出三项菜单：`打开`（默认程序打开该项）、`打开路径`（文件管理器打开其父目录）、
`复制完整路径和文件名`（写入剪贴板）。其中“打开路径”通过标准 `org.freedesktop.FileManager1.ShowItems` 打开父目录并**选中该文件/目录**（Nautilus/Dolphin/Nemo/Caja/Thunar 4.18+ 均支持），并以各文件管理器的 `--select` 作为回退。实现位于 `crates/everything-gtk/src/main.rs` 的
`connect_button_press_event` 与 `file_uri`/`open_target`/`open_parent`/`copy_path` 辅助函数。

## 中文输入法（IME）支持

GUI 是标准 GTK3 应用，通过 GTK IM 模块使用系统输入法，本身不需要特殊处理：

- 需要系统安装输入法框架及 GTK3 模块（如 `fcitx5` + `fcitx5-gtk`，或 `ibus` + `ibus-gtk3`）；
- 会话环境需导出 `GTK_IM_MODULE`、`XMODIFIERS`（fcitx 为 `GTK_IM_MODULE=fcitx`、`XMODIFIERS=@im=fcitx`）以及可用的 `DBUS_SESSION_BUS_ADDRESS`；
- 启动后搜索框会自动获得焦点（`entry.grab_focus()`），从任务栏恢复或单实例唤起时也会重新聚焦，保证输入法立即可用。

程序对输入法的处理与文件管理器/桌面环境无关，分两层自愈：

1. **GTK 初始化前**：若 `GTK_IM_MODULE` 为空，扫描 `/proc` 探测运行中的 `fcitx5`/`fcitx`/`ibus-daemon`，
   自动补上 `GTK_IM_MODULE`、`XMODIFIERS`、`QT_IM_MODULE`；
2. **GTK 初始化后**：检查实际加载的输入法上下文，若仍是 `gtk-im-context-simple`（说明没有加载任
   何输入法模块），则补全环境变量并**重新 exec 自身一次**（带 `EVERYTHING_IM_REEXEC=1` 防循环）。

因此从 Thunar、Nautilus、Dolphin、Caja、Nemo、PCManFM 等任意文件管理器双击启动，都能使用中文输入法。
唯一前提是系统已安装并运行了输入法框架（fcitx5 或 ibus）及其 GTK3 模块。

若仍无法切换：确认焦点在搜索框、`fcitx5-remote -t` 能切换，并用 `fcitx5-diagnose` 检查 GTK 前端。

### 默认索引根跟随启动位置（v0.7 修正）

- GUI/CLI 启动时把自身可执行文件路径以 `--frontend-exe` 传给守护进程，守护进程据此把**启动程序所在目录**设为默认索引根。
- 程序被移动到新目录后，再次启动会自动把根切换到新目录并重建索引；即使旧托盘实例仍在运行，也会通过单实例 socket 收到新路径并让守护进程切换根。
- 通过 `everything index add/remove` 或手写 `roots` 显式指定后，配置标记 `auto_root=false`，索引根不再随程序移动变化。
- 独立运行 `everythingd`（无 GUI/CLI 前端）时，默认索引其**当前工作目录**。
