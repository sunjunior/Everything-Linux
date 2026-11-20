# Changelog

发布流程：在 `main` 上为即将发布的 tag 增加 `## [vX.Ybeta] - YYYY-MM-DD` 小节并提交，然后打 tag。CI 会提取与 tag 同名的小节，追加构建产物说明与 SHA256，作为 GitHub Release 说明。

## [Unreleased]

## [v0.2beta] - 2026-11-20

### 亮点

- 106 万条目（93.6 万文件 + 12.4 万目录）实测：守护进程 RSS 从 441–468 MB 降至 **112–114 MB**，约 105 B/条目，与 Windows Everything 1.4 官方 100 万文件约 100 MB 处于同一水平。
- 冷索引 2.7 s，重启到可查询 0.9 s，快照 25 MB。
- 查询（引擎侧）：子串 8–12 ms、通配符 14 ms、按大小排序 11 ms、全路径 60–65 ms。

### 新增与改进

- 重构为单一紧凑索引：每条 `Entry` 固定 48 字节，删除可变 `Store` 与不可变 `IndexSnapshot` 双份索引以及两套元数据。
- 名字/折叠名使用连续字节池，父子关系改为 `first_child`/`next_sibling` 链；移除 `DashMap`、`HashMap<parent, Vec<child>>` 与 `by_name`/`by_name_cs` 排序数组。
- 查询改为按 slot 分块并行扫描，命中结果再排序；`memchr::memmem::Finder` 每次查询只构造一次；常见通配符（`*.ext`、`prefix*`、`*substr*`）走快路径；全路径查询复用缓冲区。
- 快照格式升级到 v3：流式 zstd 读写、兼容 v1/v2、启动时校验偏移并拒绝损坏快照。
- 快照/socket 等内部路径在扫描与 inotify 事件中排除；仅 generation 变化时保存，保存后 `malloc_trim(0)`，保存不再触发索引更新。
- 修复非 UTF-8 文件名/根路径在路径重建和快照加载中的损坏。
- 文档更新：README、`docs/BENCHMARKS.md`、`docs/INOTIFY_DESIGN.md`、`docs/GUI_AND_BUILD.md`。

### 修复

- **移动可执行文件后未重建索引**：恢复 `auto_root` 行为；`Engine::set_auto_root` 不再自行写配置，由守护进程在切换成功后持久化。
- `everythingd --root` / `--socket` 改为一次性覆盖，不再把 `auto_root=false` 写回用户配置、导致后续移动程序无法自动换根。
- 新增 3 个回归测试覆盖「移动前端 → 新目录重建」、重复调用幂等与手动模式。

### 升级说明

- 直接替换二进制即可；旧的 v1/v2 快照会在首次启动时自动迁移为 v3。
- 如果曾手动运行过 `everythingd --root`，请确认 `~/.config/everything-linux/config.toml` 中 `auto_root = true` 且 `exe_path` 指向当前 GUI；本版本起一次性 `--root`/`--socket` 不再修改配置。
- 移动 GUI 后，下一次启动会把新目录作为索引根并重建（需 `auto_root = true`）。

### 已知限制

- 全路径查询约 60–65 ms，慢于文件名查询。
- inotify 需要为每个目录建立 watch，受 `fs.inotify.max_user_watches` 限制；溢出时全量重建。
- 仅提供 Linux x86-64 构建；GUI 依赖系统 GTK3；部分 Wayland 会话可能不显示托盘图标。
- 尚不支持按文件内容搜索。

### 测试

- `cargo test --offline`：29 项通过（store 8、persist 3、query 10、incremental 5、auto_root 3）。

## [v0.1beta] - 2026-11-19

### 首次发布

Linux 上仿 voidtools Everything 的文件名搜索工具：常驻索引、输入即出结果、实时增量更新。

### 新增功能

- 并行目录遍历建索引；inotify 实时增量监听，watch 失败/队列溢出时子树重扫或全量重建。
- zstd 快照持久化与原子替换，重启秒开；周期性对账兜底。
- 查询语法：子串、通配符 `*`/`?`、正则、大小写/全词/全路径、AND/OR/NOT 与括号；`startwith:`、`endwith:`、`exact:`、`regex:`、`path:`、`file:`、`folder:` 等前缀。
- 属性过滤：`size:`、`dm:`（修改时间）、`dc:`（创建时间）、`da:`（访问时间）、`ext:`。
- 排序：名称、路径、大小、修改时间；结果数限制。
- 守护进程 + Unix socket IPC；静态 CLI；GTK3 GUI。
- GUI：托盘图标（关闭隐藏、双击恢复、右键退出）、单实例唤起、结果右键菜单（打开 / 打开路径 / 复制完整路径和文件名）、搜索框启动与恢复时自动聚焦。
- 单文件分发：GUI 内嵌守护进程，首次运行解压到缓存；仅拷贝一个可执行文件即可使用。
- 默认索引根跟随启动程序所在目录；移动程序后自动切换（含托盘已运行实例通知守护进程）。
- 中文输入法自愈：从 Thunar 等未携带 `GTK_IM_MODULE`/`XMODIFIERS` 的文件管理器启动时，自动探测 fcitx5/ibus 并补全环境，必要时重新 exec。
- 打开路径通过 `org.freedesktop.FileManager1.ShowItems` 选中文件，并对 nautilus/dolphin/nemo/caja 提供 `--select` 回退。
- GitHub Actions 自动构建静态 CLI/守护进程与 GTK GUI，并发布 x86-64 tarball。
- 文档：README、架构/依赖/基准/inotify 设计说明、依赖检查脚本与功能截图。

### 修复（相对基线）

- 守护进程先绑定 IPC socket 再扫描，客户端启动期不再阻塞。
- 显式 `--root` 优先于可执行文件目录；托盘实例或已运行守护进程也能正确切换根。
- 陈旧守护进程自动重启，避免旧协议/旧根。
- 托盘图标双击误判（400 ms / 8 px 判定）。
- 从 Thunar 双击启动后无法使用中文输入法。
- 搜索框在启动和窗口恢复时未自动聚焦。
- 「打开路径」只打开父目录，未选中目标文件。

### 性能（10 万文件基准）

- 子串 3.3 ms、通配符 8.1 ms、正则 13.0 ms、全路径 53.3 ms、按大小排序 10.9 ms。
- 冷索引 1–3 s；快照 716 KB；守护进程 RSS 约 42 MB（约 400 B/条目）。
- 已知内存随条目线性增长，百万级预计约 420 MB；v0.2beta 已优化到约 112 MB。

### 已知限制

- 百万级内存占用偏高（v0.2beta 修复）。
- 仅 Linux x86-64；GUI 依赖 GTK3；部分 Wayland 会话托盘不可见。
- 尚不支持按文件内容搜索。
