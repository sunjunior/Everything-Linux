# Everything-Linux

一个行为上仿照 voidtools Everything 的 Linux 文件名搜索工具：常驻内存索引、输入即出结果、实时增量更新。

## 构建（完全离线）

```bash
cd /home/sunjunior/work/everything-research/everything-linux
cargo build --offline --release
cargo test --offline
```

产物：`target/release/everythingd`（守护进程）、`target/release/everything`（CLI 客户端）、`target/release/everything-gtk`（GTK3 GUI）。

分发：`./build-static.sh` → `dist/`。`everything-gtk` 已内嵌守护进程，**单独拷这一个文件到目标机器即可双击使用**（首次运行自动释放守护进程并以 GUI 所在目录为索引根）；CLI 与 daemon 仍为独立 static-pie 文件。详见 `docs/GUI_AND_BUILD.md`。

**跨机器部署前请阅读 `docs/DEPENDENCIES.md`**（glibc / GTK3 / 图标主题 / 内核版本要求），并在目标机运行 `./scripts/check-deps.sh ./dist` 自检。

## 快速开始

```bash
# 配置文件（不存在时首次启动自动以当前程序所在目录为索引根）
mkdir -p ~/.config/everything-linux
cat > ~/.config/everything-linux/config.toml <<'EOF'
# roots 留空时首次启动自动写入当前可执行文件所在目录；
# 也可显式写 roots = ["/path/one", "/path/two"]（显式配置不会随程序移动而改变）
snapshot_path = "~/.cache/everything-linux/db.eldb"   # 配置中的路径支持 ~ 展开为主目录
socket_path = "/tmp/everything-linux.sock"
watch_enabled = true
debounce_ms = 15
rescan_interval_secs = 600
snapshot_interval_secs = 5
threads = 8
max_results = 100000
EOF

# 启动（首次会全量扫描；之后从快照秒开）
./target/release/everythingd

# 查询
./target/release/everything report.pdf
./target/release/everything '*.rs'
./target/release/everything -e '^test_[0-9]+'
./target/release/everything '*.pdf | *.docx'
./target/release/everything '*.tmp !keep'
./target/release/everything --path 'projects/src'
./target/release/everything -s -size -l 20
./target/release/everything --json 'query' | jq .

# 属性过滤（新增）
./target/release/everything 'size:>10mb'
./target/release/everything 'dm:>2025-01-01'
./target/release/everything 'ext:rs|md'
```

CLI 在守护未运行时会尝试从同级目录自动拉起 `everythingd`。

## 命令

| 命令 | 说明 |
|---|---|
| `everything status` | 索引状态（文件数、目录数、generation、watch 数、根列表） |
| `everything index add <path>` | 添加索引根并扫描（显式配置后不再随程序路径变化） |
| `everything index list` | 同 `status` |
| `everything index remove <path>` | 移除索引根及其条目 |
| `everything reindex` | 清空并重建全部索引 |
| `everything save` | 立即保存快照 |
| `everything shutdown` | 停止守护 |

## 守护参数

```
-r, --root <PATH>       索引根（可重复；不指定则默认当前工作目录）
    --no-snapshot       忽略快照，强制全量重建
    --no-watch          关闭 inotify
    --threads <N>       扫描线程数
    --socket <PATH>     IPC socket 路径
-q, --quiet             关闭日志
```

## 说明与限制

详见 `../RESEARCH_REPORT.md` 与 `../TECH_DESIGN.md`。当前版本已包含文件名搜索、属性过滤与 GTK3 GUI；未实现内容搜索、HTTP/ETP、重复文件检测。

## 结果右键菜单

在搜索结果上右键，提供三个常用操作（与 Everything 类似）：

- **打开**：用系统默认程序打开选中的文件/目录（等同双击）；
- **打开路径**：用文件管理器打开该项所在的父目录；
- **复制完整路径和文件名**：把完整路径写入系统剪贴板。
