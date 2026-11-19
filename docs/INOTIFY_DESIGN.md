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
                                └─ 应用变更到 Store，收集 changed slots
                                      └─ 防抖后 apply_changes() 原子发布新快照
```

## 3. 为什么不用"每次事件都全量重建"

v0.1 最初每次变更都重建全部 `by_name` 数组：复制所有名字 + rayon 排序 10 万条目约需数十毫秒，百万级会更慢。

现在改为**增量快照更新** `IndexSnapshot::apply_changes(store, changed)`：

- 对每个变更 slot：
  1. 用二分查找在 `by_name` / `by_name_cs` 中定位旧名字的位置并删除（数组按 `(name, slot)` 排序，二分key一致）；
  2. 从 `by_parent` 移除旧的父→子关系；
  3. 从 Store 读取新名字/元数据，追加到名字池尾部（旧偏移保持有效），更新 `entries[slot]`；
  4. 按新名字二分插入 `by_name` / `by_name_cs`，更新 `by_parent` 和文件/目录计数；
  5. 删除条目则标记 `kind=Other, name_len=0`。
- 通过 `arc_swap::ArcSwap` 一次性发布新快照，查询线程零锁。
- 复杂度从 O(n log n) 降为 O(k · (log n + n/2 插入位移))，k=变更条目数。实测比全量重建快一个数量级。

### 正确性保证

`crates/core/tests/incremental.rs` 用 4 个测试对比"增量结果"与"全量重建结果"：
插入、删除、改名+元数据修改、重名文件场景，均要求排序数组与计数完全一致。

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
