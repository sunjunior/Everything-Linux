# 为什么选 inotify，以及增量索引是怎么做的

## 1. inotify 是不是 Linux 上合适的机制？

**结论：对"文件名索引"这个场景，inotify 是正确选择；fanotify 不是替代品。**

| 维度 | inotify | fanotify |
|---|---|---|
| 权限 | 普通用户可用 | 通常需要 `CAP_SYS_ADMIN` |
| 事件粒度 | 目录项级别：`CREATE/DELETE/MOVED_FROM/MOVED_TO/MODIFY/ATTRIB` | 面向文件访问/打开的审计与拦截，权限判定模型 |
| 与索引需求匹配 | 直接给出父目录 + 名字 + cookie，正好映射 `(parent,name)` | 不提供可靠的"目录树新增/删除文件名"流 |
| 开销 | 每个被监视目录一个 watch；大量目录时受 `max_user_watches` 限制 | 挂载点级，watch 数少但语义不同 |
| 持久性 | 无，进程停止后事件丢失 | 无 |
| 典型用途 | 文件同步、索引、编辑器 | 杀毒、DLP、访问控制 |

本机 `/proc/sys/fs/inotify/max_user_watches = 524288`，对桌面场景足够；项目仍实现了超限兜底。

补充机制（Linux 上没有 USN Journal 的等价物）：
- `inotify` 负责"运行时实时"；
- **zstd 快照**负责"重启后秒开"；
- **周期性对账 + 溢出全量重建**负责"补上遗漏事件"。

这三者合起来才等价于 Everything 的「MFT 全扫 + USN 增量」。

## 2. 事件到索引的完整路径

```
libc inotify fd (非阻塞)
  └─ reader 线程：解析 inotify_event（wd/mask/cookie/name）
       ├─ MOVED_FROM(cookie) → 暂存
       ├─ MOVED_TO(cookie)   → Rename{from,to}
       ├─ CREATE / IS_DIR    → CreateDir / Create
       ├─ DELETE / DELETE_SELF → Delete
       ├─ MODIFY / CLOSE_WRITE / ATTRIB → Modify（重取 size/mtime）
       ├─ IGNORED / MOVE_SELF → Rescan（重挂 watch + 子树对账）
       ├─ IN_Q_OVERFLOW / UNMOUNT → Overflow（全量重建）
       └─ crossbeam channel → Engine::tick()
                                └─ 防抖后直接增量修改 Store（不复制索引）
                                      └─ 递增 generation，按需落盘快照
```

## 3. 为什么不再维护"快照副本"

v0.1 同时保留可变 `Store` 和不可变 `IndexSnapshot`，每次变更都要复制名字池、重建排序数组并通过 `ArcSwap` 发布。百万级时两份索引常驻约 300 MB，且快照写入会产生数十 MB 的临时缓冲。

v0.2 改为**单一紧凑索引**：

- 每条 `Entry` 固定 48 字节，名字和折叠名存在连续字节池中，不再有 `EntryMeta`/`SnapEntry` 两份元数据；
- 父目录用 `first_child` / `next_sibling` 链访问，删除了 `DashMap`、`HashMap<parent, Vec<child>>` 和 `by_name` / `by_name_cs` 排序数组；
- inotify 事件在写锁内直接修改 `Store`，防抖结束后只递增 generation，不再复制整个索引；
- 查询在 `RwLock` 读锁下按 slot 顺序并行扫描，只对命中结果排序，因此不需要维护全局排序数组；
- 名字折叠与子串查找复用缓冲区，`memchr::memmem::Finder` 每次查询只构造一次；
- 快照仅在 generation 变化时按 `snapshot_interval_secs` 流式写入 zstd，保存后调用 `malloc_trim(0)` 归还页；
- 快照文件、临时文件与 socket 等内部路径在扫描和事件处理中都会被排除，保存快照不会触发新的索引更新。

实测 106 万条目：守护进程 RSS 112–114 MB、快照 25 MB、冷索引 2.7 s、重启到可查询 0.9 s。

### 正确性保证

`crates/core/tests/incremental.rs` 用 5 个测试覆盖插入、删除、改名、重名排序与批量插入；`store_layout.rs` 校验 48 字节布局、折叠名复用、父子链、路径重建与计数；`persist.rs` 校验 v3 快照往返与 v2 旧快照兼容。

## 4. 事件可靠性兜底

| 问题 | 处理 |
|---|---|
| 新目录出现 | `add_watch_recursive` 立即为新目录及其子目录挂 watch，并扫描已有内容 |
| `ENOSPC`（watch 数上限） | 记录 `WatchFailed(path)`，加入 `rescan_paths`，周期性对账该子树 |
| `IN_Q_OVERFLOW` | 事件流不可信 → 立即 `reindex()` 全量重建 |
| `IN_IGNORED` / `IN_MOVE_SELF` | 重挂 watch + 子树对账 |
| 守护进程停机期间的变更 | 启动时加载快照秒开，再扫描根目录补漏 |
| 长时间运行可能漏事件 | `rescan_interval_secs`（默认 600s）触发周期性全量对账；设为 0 关闭 |

## 5. 关键配置

```toml
watch_enabled = true
debounce_ms = 15              # 事件聚合窗口，越小越实时
rescan_interval_secs = 600    # 周期性兜底对账；0 关闭
snapshot_interval_secs = 5    # 快照落盘节流，避免每次变更都写盘
threads = 8
```

`status` 命令输出 `full_rebuilds` 与 `incremental_updates` 计数，可验证是否走增量路径。
