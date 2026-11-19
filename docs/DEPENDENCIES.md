# 运行依赖与跨机器部署兼容性

本文说明 `dist/` 中三个可执行文件在**另一台 Linux x86-64 机器**上运行所需的全部条件。
所有结论均来自对实际产物执行 `file` / `readelf` / `ldd` 的结果，而非推测。

## 1. 产物总览

| 文件 | 链接方式 | 大小 | 用途 |
|---|---|---|---|
| `dist/everything` | static-pie（静态链接） | 1.8 MB | CLI 查询客户端 |
| `dist/everythingd` | static-pie（静态链接） | 4.7 MB | 索引守护进程 |
| `dist/everything-gtk` | 动态链接 | 816 KB | GTK3 图形界面 |

构建环境（参考值）：Arch Linux x86-64，内核 7.2.9，glibc 2.44，GTK 3.24.52。

## 2. 通用硬件 / 内核要求（三个文件都适用）

| 条件 | 要求 | 依据 |
|---|---|---|
| CPU 架构 | **x86-64（64 位）** | ELF `x86-64` |
| 操作系统 | Linux | — |
| 内核版本 | **≥ 4.4** | ELF `.note.ABI-tag` 标记为 `GNU/Linux 4.4.0` |
| 权限 | 普通用户即可；不需要 root | 索引用户可访问的目录 |
| 文件系统 | 任意 Linux 文件系统；实时增量依赖内核 inotify（≥ 2.6.13） | `IN_CREATE`/`IN_DELETE`/`IN_MOVED_*` |

> 32 位 x86、ARM、armv7 手机（例如 ZTE Grand S）**不能直接运行**这些二进制，需要在目标架构上重新编译。

## 3. CLI 与守护进程（静态二进制）

`readelf -d dist/everything` 与 `dist/everythingd` 均**没有任何 `NEEDED` 条目**：

```
$ readelf -d dist/everything | grep NEEDED
（无输出）
```

因此：

- **不依赖目标机器的 glibc 版本**（glibc 已静态编入）；
- **不依赖 GTK / GLib / X11 / Wayland / 图标主题 / 字体**；
- 可直接在干净的 x86-64 Linux（甚至容器 / _busybox 环境）运行；
- 仍需要内核 ≥ 4.4 以及标准 `/proc`、`/tmp`（或 `XDG_RUNTIME_DIR`）可写。

### 运行时目录与环境变量

| 用途 | 默认路径 | 说明 |
|---|---|---|
| 配置 | `$XDG_CONFIG_HOME/everything-linux/config.toml`（默认 `~/.config/...`） | 首次启动自动写入当前可执行文件所在目录作为索引根 |
| 快照数据库 | `$XDG_CACHE_HOME/everything-linux/db.eldb`（默认 `~/.cache/...`） | 父目录会自动创建 |
| IPC socket | `$XDG_RUNTIME_DIR/everything-linux.sock`，无则 `/tmp/everything-linux.sock` | CLI / GUI 与守护通信 |

`HOME`、`XDG_CONFIG_HOME`、`XDG_CACHE_HOME`、`XDG_RUNTIME_DIR` 至少需要一个可用；否则部分路径会退化到 `/tmp` 或当前目录。

### 大规模索引的 Tunables

- `/proc/sys/fs/inotify/max_user_watches`：每个被监视目录消耗一个 watch；建议 ≥ 索引目录数（本机为 524288）。
- `/proc/sys/fs/inotify/max_queued_events`：事件队列过小会触发 `IN_Q_OVERFLOW`（程序会自动全量重建兜底）。

## 4. GTK GUI 的依赖

### 4.1 直接链接库（`readelf -d` 的 NEEDED）

```
libgobject-2.0.so.0
libgtk-3.so.0
libgdk-3.so.0
libgio-2.0.so.0
libglib-2.0.so.0
libgcc_s.so.1
libc.so.6
```

### 4.2 版本下限（关键）

| 依赖 | 构建/验证版本 | 兼容性说明 |
|---|---|---|
| glibc | 构建 2.44；动态符号含 `GLIBC_2.2.5` … **`GLIBC_2.39`** | `pidfd_spawnp` / `pidfd_getpid` 是 **2.39 引入的弱符号**；为行为一致**建议目标 glibc ≥ 2.39**。旧版 glibc 通常会忽略缺失的弱符号，但本项目未在旧版上验证，不保证 GUI 可用。 |
| GTK | **GTK 3**（构建 3.24.52） | **GTK 2 不兼容**；未验证更早的 3.x，建议 3.24 或更新 |
| GLib / GObject / GIO | 构建 2.90.1 | 建议 ≥ 2.70（老版本的 API 可能缺失） |
| Pango | 构建 1.58.2 | 文本渲染 |
| Cairo | 构建 1.18.6 | 绘图后端 |
| GDK-Pixbuf | 构建 2.44.8 | 图标加载 |
| ATK | 构建 2.62.0 | 无障碍接口 |

### 4.3 完整传递依赖（`ldd` 实测需要的 soname）

GTK 栈会连带拉入以下库；目标机器缺少其中任何一个都会导致 GUI 无法启动：

- GLib 系：`libglib-2.0.so.0`、`libgobject-2.0.so.0`、`libgio-2.0.so.0`、`libgmodule-2.0.so.0`
- GTK/GDK：`libgtk-3.so.0`、`libgdk-3.so.0`、`libgdk_pixbuf-2.0.so.0`
- 文本/绘图：`libpango-1.0.so.0`、`libpangocairo-1.0.so.0`、`libpangoft2-1.0.so.0`、`libcairo.so.2`、`libcairo-gobject.so.2`、`libharfbuzz.so.0`、`libfribidi.so.0`、`libfontconfig.so.1`、`libfreetype.so.6`、`libthai.so.0`、`libdatrie.so.1`、`libgraphite2.so.3`、`libpng16.so.16`、`libpixman-1.so.0`、`libexpat.so.1`
- X11/Wayland：`libX11.so.6`、`libxcb.so.1`、`libxcb-render.so.0`、`libxcb-shm.so.0`、`libXext.so.6`、`libXfixes.so.3`、`libXrender.so.1`、`libXi.so.6`、`libXcursor.so.1`、`libXdamage.so.1`、`libXcomposite.so.1`、`libXrandr.so.2`、`libXinerama.so.1`、`libXau.so.6`、`libXdmcp.so.6`、`libxkbcommon.so.0`、`libwayland-client.so.0`、`libwayland-cursor.so.0`、`libwayland-egl.so.1`、`libepoxy.so.0`
- 其他：`libatk-1.0.so.0`、`libatk-bridge-2.0.so.0`、`libatspi.so.0`、`libdbus-1.so.3`、`libjson-glib-1.0.so.0`、`libxml2.so.16`、`libsqlite3.so.0`、`libsystemd.so.0`、`libmount.so.1`、`libblkid.so.1`、`libz.so.1`、`libbz2.so.1.0`、`libbrotlidec.so.1`、`libbrotlicommon.so.1`、`libseccomp.so.2`、`libffi.so.8`、`libgcc_s.so.1`、`libm.so.6`、`libc.so.6`、`libstdc++.so.6`、`libtinysparql-3.0.so.0`、`libglycin-2.so.0`、`libcloudproviders.so.0`、`libicuuc.so.78`、`libicudata.so.78`

> 注：部分传递库（如 `libtinysparql`、`libglycin`、`libcloudproviders`）来自较新发行版的 GIO 依赖链；在更精简的系统上，相应 soname 可能不存在，但这通常不影响本程序使用的 GTK 基础功能。最可靠的判断方式是直接运行 `ldd dist/everything-gtk` 并检查是否出现 `not found`。

### 4.4 显示环境 / 任务栏图标

- 需要可用的图形会话：`DISPLAY`（X11）或 `WAYLAND_DISPLAY`（Wayland）。
- **任务栏图标**通过 GTK3 `GtkStatusIcon` 实现：
  - X11 桌面（带系统托盘 / notification area）通常可正常显示；
  - 纯 Wayland 会话或没有托盘宿主的环境（例如部分 GNOME 默认会话）中图标可能**不显示**，这只影响驻留/双击恢复，不影响搜索功能；
- 图标取自系统图标主题，按顺序寻找 `system-search` → `edit-find` → `gtk-find` → `applications-system`。建议安装 `adwaita-icon-theme` / `hicolor-icon-theme` 等基础图标主题，否则图标可能空白。
- 文本渲染需要 fontconfig 与至少一套可用字体。

## 5. 目标机器自查脚本

随附 `scripts/check-deps.sh`，拷到目标机器后运行：

```sh
chmod +x check-deps.sh
./check-deps.sh /path/to/dist
```

它会检查：CPU 架构、内核版本、静态二进制有效性、GUI 的 `ldd not found`、glibc 版本、GTK3 与图标主题、X11/Wayland 环境变量。

也可手动快速检查：

```sh
file dist/everything dist/everythingd dist/everything-gtk
ldd dist/everything-gtk | grep 'not found'   # 应无输出
readelf -d dist/everything | grep NEEDED      # 应无输出
./dist/everything --help
./dist/everythingd --help
```

## 6. 发行版兼容性速查（参考）

| 系统 | CLI / daemon（静态） | GUI（动态） |
|---|---|---|
| Arch Linux（当前验证环境） | 可用 | 可用 |
| Ubuntu 24.04（glibc 2.39 + GTK3） | 可用 | 预计可用（建议实测） |
| Debian 12（glibc 2.36） | 可用 | **不保证**（低于 2.39，需实测） |
| RHEL / CentOS 8（glibc 2.28） | 可用 | **不保证** |
| Alpine / musl | 不可用（glibc 静态二进制不兼容 musl） | 不可用 |

> 最稳妥的做法：把 `dist/` 拷到目标机器后运行 `scripts/check-deps.sh` 并启动一次 GUI；若 GUI 依赖不满足，仍可完全使用 CLI + daemon（两者无任何外部库依赖）。

## 7. 部署清单

1. 确认 x86-64 + 内核 ≥ 4.4；
2. 拷贝 `dist/everything` 与 `dist/everythingd`（必要时加 `dist/everything-gtk`）到目标机；
3. 运行 `check-deps.sh`；
4. 首次运行 `./everythingd`（默认索引当前工作目录，或用 `--root` / 配置文件指定）；
5. 用 `./everything status` 确认索引正常；
6. 如需 GUI，确认图形会话、GTK3、图标主题、任务栏宿主可用后运行 `./everything-gtk`。
