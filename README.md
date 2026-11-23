# Everything-Linux

<p align="center"><img src="crates/everything-gtk/assets/icon-256.png" width="128" alt="Everything-Linux icon"></p>

一个行为上仿照 voidtools Everything 的 Linux 文件名搜索工具：**常驻内存索引、输入即出结果、实时增量更新**。

## 背景

Everything 在 Windows 上的极速来自两个系统事实：直接解析 NTFS MFT 拿到全卷文件名，再用 USN Change Journal 做增量更新。Linux 没有等价的稳定机制（各文件系统元数据布局不同，无持久变更日志），因此本项目改用：

- 并行目录树遍历建索引；
- `inotify` 监听实时变化（watch 失败/溢出时子树重扫或全量重建兜底）；
- zstd 快照实现重启秒开与周期对账；
- 48 字节紧凑条目 + 名字池 + 并行布尔条件图查询引擎，复刻 Everything "查询时只做一次顺序扫描" 的核心思路。

## 功能

- 百万级文件名常驻索引，默认按名称排序，查询近乎即时
- 查询语法：子串、通配符 `*`/`?`、正则、大小写/全词/路径开关、AND/OR/NOT 与括号
- 属性过滤：`size:`、`dm:`、`dc:`、`da:`、`ext:`
- 实时增量：创建/删除/重命名/修改/目录子树，inotify 溢出自动全量重建
- 持久化：zstd 快照，原子替换，配置/数据走 XDG 标准路径
- 融合 GUI：索引、查询、inotify 全在 `everything-gtk` 进程内，无 daemon、无 IPC、无 Unix socket
- 实时刷新：输入关键词后，文件新增/删除/原子替换会自动刷新结果列表（250ms 防抖，保留选中行）
- GUI：Everything 风格橙色放大镜图标（窗口 + 托盘）、任务栏图标（关闭窗口隐藏到托盘、双击恢复、右键退出）、单实例（重复启动自动唤起已有窗口）
- 结果右键菜单：`打开`、`打开路径`（打开父目录并选中该项）、`复制完整路径和文件名`、`删除`（文件/空目录，二次确认）
- 单文件分发：发布包只含一个约 3.6 MB 的 `everything-gtk`，需要系统 GTK3

## 界面预览

以下截图使用合成测试数据，不包含任何真实路径或个人信息；状态栏中的运行时统计信息已做马赛克处理。

| 搜索结果（输入即出） | 属性过滤（`ext:md`） | 结果右键菜单 |
|:---:|:---:|:---:|
| ![搜索结果](docs/screenshots/01-search-results.png) | ![属性过滤](docs/screenshots/02-property-filter.png) | ![右键菜单](docs/screenshots/03-context-menu.png) |
| 文件名即时匹配 | `size:` / `dm:` / `ext:` 等过滤 | 打开 / 打开路径 / 复制完整路径和文件名 / 删除 |

## 软件架构

```
[walker 并行遍历] ─┐
                   ├─→ [Store: 48B 紧凑条目 + 名字池] ─→ [并行查询引擎: term 图 + 特化求值]
[monitor inotify] ─┘              ▲                              │
       ▲                          │ 增量修改                      │
       │ 事件合并/溢出兜底          └──────────────────────────────┘

融合架构 GUI： [GTK3] ─→ [LocalEngine] ─→ 同进程索引 / 查询 / tick
```

工程结构：

```
crates/
  everything_core/   纯引擎库（model/store/index/walker/query/monitor/persist/ipc/engine/config/sdk/raw）
  everythingd/       可选守护进程：索引 + inotify + IPC server
  everything/       可选 CLI 客户端
  everything-gtk/    融合 GTK3 GUI；索引在进程内，build.rs 不再内嵌 daemon
docs/                BENCHMARKS / INPROCESS_BENCH / INOTIFY_DESIGN / GUI_AND_BUILD / DEPENDENCIES
scripts/check-deps.sh  目标机依赖自检
build-static.sh      CLI/daemon 静态构建（融合 GUI 发布不走此脚本）
```

关键设计：只有一份紧凑索引。每条 `Entry` 固定 48 字节，名字（及必要的折叠名）存于连续字节池，父目录用 `first_child`/`next_sibling` 链访问；inotify 事件直接增量修改索引并递增 generation。融合 GUI 通过 `LocalEngine` 在进程内持有引擎，查询走 slot + 惰性路径渲染，结果列表只保留可见行所需的轻量字段；换根时后台构建新索引，旧索引继续服务查询，完成后原子切换。

## 构建

本地构建需要 Rust 1.85+（edition 2024）与 Cargo：

```bash
git clone https://github.com/sunjunior/Everything-Linux.git
cd Everything-Linux
cargo test --offline
cargo build --offline --release -p everything-gtk
# 产物：target/release/everything-gtk
```

融合 GUI 发布由 `.github/workflows/build-fused.yml` 完成：在版本标签上构建 Linux x86_64 / aarch64 / armv7，每个压缩包只放一个 `everything-gtk`。x86_64 与 aarch64 使用原生 runner，armv7 使用 QEMU 容器；不再构建或内嵌 daemon。

`build-static.sh` 仍可用于本地生成 CLI/daemon 静态产物，但融合发布不使用它：

| 文件 | 说明 |
|---|---|
| `dist/everything-gtk` | 融合 GUI，索引在进程内，单文件约 3.6 MB |
| `dist/everything` | 可选 CLI 客户端，static-pie |
| `dist/everythingd` | 可选守护进程，static-pie |

## 发布与变更记录

每个版本的功能、修复与升级说明见 [CHANGELOG.md](CHANGELOG.md)。发布 tag 前先在该文件增加与 tag 同名的小节（例如 `## [v0.2beta] - 2026-11-20`）；CI 会提取该小节并追加构建产物与 SHA256，作为 GitHub Release 说明。

## 部署与运行依赖

**最简方式**：把 `dist/everything-gtk` 单独拷到目标机器，双击/直接运行即可。索引、查询和 inotify 都在这个进程内完成，不需要 daemon、CLI 或 socket 文件；GUI 所在目录是默认索引根，移到新目录后再启动会自动后台换根。

- CLI/daemon 为静态链接：不依赖目标机 glibc 版本、GTK、X11；只需 x86-64 + Linux 内核 ≥ 4.4。
- GUI 动态依赖系统 **GTK3**（`libgtk-3.so.0`、`libglib-2.0.so.0` 等）；动态符号含 `GLIBC_2.39`，建议 glibc ≥ 2.39；需 X11/Wayland 图形会话和图标主题（任务栏图标）。
- 完整依赖清单、兼容性与自检脚本见 [`docs/DEPENDENCIES.md`](docs/DEPENDENCIES.md)。

配置路径：`~/.config/everything-linux/config.toml`（路径支持 `~` 展开）：

```toml
# roots 留空时自动跟随启动程序（everything-gtk / everything）所在目录；
# 程序移动到新目录后，下次启动会自动切换为新目录（即使旧托盘实例仍在运行）；
# 显式写 roots = ["/path/one"] 或 index add/remove 后则固定，不再跟随
snapshot_path = "~/.cache/everything-linux/db.eldb"
socket_path = "/tmp/everything-linux.sock"
watch_enabled = true
debounce_ms = 15
rescan_interval_secs = 600
snapshot_interval_secs = 5
threads = 8
max_results = 100000
```

## 使用

```bash
# 融合 GUI（推荐；单进程，无需先启动 daemon）
./dist/everything-gtk

# 可选：独立守护 + CLI 查询（默认索引当前工作目录，或用 --root 指定）
./dist/everythingd --root ~/Documents
./dist/everything report.pdf
./dist/everything '*.rs'
./dist/everything -e '^test_[0-9]+'
./dist/everything '*.pdf | *.docx'
./dist/everything '*.tmp !keep'
./dist/everything --path 'projects/src'
./dist/everything 'size:>10mb'
./dist/everything 'dm:>2025-01-01'
./dist/everything 'ext:rs|md'
./dist/everything --json 'query' | jq .

# 管理
./dist/everything status
./dist/everything index add ~/Music
./dist/everything index remove ~/Music
./dist/everything reindex
./dist/everything shutdown

```

GUI 行为：关闭窗口隐藏到任务栏，双击托盘图标恢复，右键托盘图标退出；重复启动会自动唤起已有窗口；结果右键可打开、打开所在目录或复制完整路径。

中文输入：GUI 使用系统 GTK3 输入法模块（fcitx5 / ibus）。即使启动它的文件管理器没有携带 `GTK_IM_MODULE`/`XMODIFIERS`（如从 Thunar 双击），程序也会在 GTK 初始化前后自动探测运行中的输入法框架、补全环境变量，必要时重新 exec 自身一次，因此与具体文件管理器/桌面环境无关。搜索框在启动和窗口恢复时自动聚焦。

## 性能（106 万条目实机）

| 指标 | 结果 |
|---|---:|
| 单进程 GUI RSS / PSS | 186 MB / 126 MB |
| GUI 可执行文件 | 3.6 MB |
| 快照大小 | 25 MB |
| 冷索引 | 2.4–2.7 s |
| 换根后台重建（不阻塞查询） | 2.4 s，查询 P95 16.4 ms |
| 子串 `*.rs` | 8.9 ms（GUI 状态栏 10.6 ms） |
| 全路径 `path:` | 51 ms |
| 按大小排序 | 7.5 ms |

与 Windows Everything 1.4 官方数据（100 万文件约 100 MB 内存、45 MB 磁盘、约 1 分钟索引）处于同一内存级别。融合后省掉了 daemon 进程和 IPC，单进程比旧 daemon + GUI 的 RSS / PSS 分别低约 4 MB / 13 MB。详见 [`docs/INPROCESS_BENCH.md`](docs/INPROCESS_BENCH.md)。


## 许可

MIT OR Apache-2.0（双许可）。
