# Everything-Linux

一个行为上仿照 voidtools Everything 的 Linux 文件名搜索工具：**常驻内存索引、输入即出结果、实时增量更新**。

## 背景

Everything 在 Windows 上的极速来自两个系统事实：直接解析 NTFS MFT 拿到全卷文件名，再用 USN Change Journal 做增量更新。Linux 没有等价的稳定机制（各文件系统元数据布局不同，无持久变更日志），因此本项目改用：

- 并行目录树遍历建索引；
- `inotify` 监听实时变化（watch 失败/溢出时子树重扫或全量重建兜底）；
- zstd 快照实现重启秒开与周期对账；
- 内存中的多排序数组 + 布尔条件图查询引擎，复刻 Everything "查询时只做一次顺序扫描" 的核心思路。

详细逆向研究见仓库外的研究文档（样本：Everything 1.4.1.1032 x64，Ghidra 静态分析 + 反编译查询编译器）。

## 功能

- 百万级文件名常驻索引，默认按名称排序，查询近乎即时
- 查询语法：子串、通配符 `*`/`?`、正则、大小写/全词/路径开关、AND/OR/NOT 与括号
- 属性过滤：`size:`、`dm:`、`dc:`、`da:`、`ext:`
- 实时增量：创建/删除/重命名/修改/目录子树，inotify 溢出自动全量重建
- 持久化：zstd 快照，原子替换，配置/数据走 XDG 标准路径
- 前后端分离：守护进程 + CLI + GTK3 GUI，通过 Unix socket IPC 通信
- GUI：任务栏图标（关闭窗口隐藏到托盘、双击恢复、右键退出）、单实例（重复启动自动唤起已有窗口）
- 结果右键菜单：`打开`、`打开路径`、`复制完整路径和文件名`
- 单文件分发：GUI 内嵌守护进程，拷贝一个文件即可使用

## 软件架构

```
[walker 并行遍历] ─┐
                   ├─→ [Store: SoA + 字符串池] ─→ [IndexSnapshot: 多排序数组] ─→ [ArcSwap 发布]
[monitor inotify] ─┘                                        │
       ▲                                                    ▼
       │ 事件合并/溢出兜底                          [Query Engine: term 图 + 特化求值]
       │                                                    │
[zstd 快照] ←─────────────────────────────── [CLI / GTK GUI / Unix socket IPC]
```

工程结构：

```
crates/
  everything_core/   纯引擎库（model/store/index/walker/query/monitor/persist/ipc/engine/config）
  everythingd/       守护进程：索引 + inotify + IPC server
  everything/        CLI 客户端
  everything-gtk/    GTK3 GUI（build.rs 内嵌 everythingd）
docs/                BENCHMARKS / INOTIFY_DESIGN / GUI_AND_BUILD / DEPENDENCIES
scripts/check-deps.sh  目标机依赖自检
build-static.sh      静态/分发构建
```

关键设计：`IndexSnapshot` 用 `Arc` 共享名字池与基础条目，增量更新只维护变更条目的 `entry_overrides` 和排序数组位置，通过 `arc_swap` 原子发布，查询线程全程无锁。

## 构建

需要 Rust 1.85+（edition 2024）与 Cargo。本项目可完全离线构建（依赖已缓存）：

```bash
git clone https://github.com/sunjunior/Everything-Linux.git
cd Everything-Linux
cargo test --offline          # 测试
cargo build --offline --release
```

生成可分发产物（CLI/daemon 为 static-pie；GUI 内嵌 daemon）：

```bash
./build-static.sh             # 产物在 dist/
```

`dist/` 内容：

| 文件 | 说明 |
|---|---|
| `everything` | CLI 客户端，static-pie，1.8 MB |
| `everythingd` | 守护进程，static-pie，4.7 MB |
| `everything-gtk` | GTK3 GUI，内嵌守护进程，单文件分发约 5.5 MB |

## 部署与运行依赖

**最简方式**：把 `dist/everything-gtk` 单独拷到目标机器，双击/直接运行即可。首次运行会把内嵌的守护进程释放到 `~/.cache/everything-linux/`，并以 GUI 所在目录作为默认索引根。

- CLI/daemon 为静态链接：不依赖目标机 glibc 版本、GTK、X11；只需 x86-64 + Linux 内核 ≥ 4.4。
- GUI 动态依赖系统 **GTK3**（`libgtk-3.so.0`、`libglib-2.0.so.0` 等）；动态符号含 `GLIBC_2.39`，建议 glibc ≥ 2.39；需 X11/Wayland 图形会话和图标主题（任务栏图标）。
- 完整依赖清单、兼容性与自检脚本见 [`docs/DEPENDENCIES.md`](docs/DEPENDENCIES.md)。

配置路径：`~/.config/everything-linux/config.toml`（路径支持 `~` 展开）：

```toml
# roots 留空时首次启动自动写入当前可执行文件所在目录；
# 显式写 roots = ["/path/one", "/path/two"] 则不再随程序移动而改变
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
# 启动守护（默认索引当前工作目录，或用 --root 指定）
./dist/everythingd --root ~/Documents

# CLI 查询
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

# GUI
./dist/everything-gtk
```

GUI 行为：关闭窗口隐藏到任务栏，双击托盘图标恢复，右键托盘图标退出；重复启动会自动唤起已有窗口；结果右键可打开、打开所在目录或复制完整路径。

## 性能（10 万文件基准）

| 查询 | 引擎耗时（P50） |
|---|---|
| 子串 | 3.3 ms |
| 通配符 | 8.1 ms |
| 正则 | 13.0 ms |
| 全路径 | 53.3 ms |
| 按大小排序 | 10.9 ms |

创建文件到查询可见约 40 ms（含 CLI 进程开销）；快照加载约 158 ms。详见 [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md)。

## 限制

未实现：文件内容搜索、HTTP/ETP 接口、重复文件检测、`root:`/`empty:` 特殊函数、变音符号折叠、EFU 文件列表索引。GUI 在无系统托盘宿主的桌面环境中图标可能不可见（搜索功能不受影响）。

## 许可

MIT OR Apache-2.0（双许可）。
