# 性能基准（v0.2.0-beta，release）

测试环境：Arch Linux x86-64，内核 7.2.9，8 核，31 GiB RAM。
数据集：`/home` 实际目录树，**935,770 个文件 + 124,062 个目录 = 1,059,832 条目**。
构建：`./build-static.sh`（release，thin LTO，8 线程索引，zstd level 1）。

## 与 Everything 1.4 官方数据对比

Everything 官方数据来自 voidtools FAQ：
25 万文件约 35 MB 内存；100 万文件约 **100 MB 内存、45 MB 磁盘、约 1 分钟冷索引**。
口径：Everything 统计“文件”，本工具统计“文件 + 目录”；内存均为进程 RSS。

| 指标 | Everything（100 万文件） | 本工具（106 万条目） |
|---|---:|---:|
| 常驻内存 RSS | 约 100 MB | **112–114 MB**（峰值 118–123 MB） |
| 快照 / 数据库 | 约 45 MB | **25 MB** |
| 冷索引 | 约 60 s | **2.7 s** |
| 重启加载到可查询 | 未公开（通常 < 1 s） | **0.9 s**（含 12.4 万 inotify watch） |
| 子串查询（引擎侧） | 即时 | **8–12 ms** |
| 通配符查询 | 即时 | **14 ms** |
| 全路径查询 | 未公开 | **60–65 ms** |
| 按大小排序 | 未公开 | **11 ms** |

折算约 **105 B/条目**，与 Everything 的约 100 B/文件处于同一水平；磁盘占用约为其一半，冷索引显著更快，查询为个位数到数十毫秒。

## 查询延迟（引擎侧，limit=10，106 万条目）

| 查询 | 耗时 | 命中数 |
|---|---:|---:|
| `zzzzz-nomatch`（全量扫描无命中） | 8.3 ms | 0 |
| `main.rs`（子串） | 9–12 ms | 77 |
| `*.rs`（通配符） | 13–14 ms | 18,499 |
| `path:everything-linux`（全路径） | 60–65 ms | 12,000+ |
| `-s -size *.txt`（通配 + 排序） | 11 ms | 5,494 |
| `ext:rs`（扩展名过滤） | 14 ms | 18,499 |

实现要点：

- 查询按 slot 顺序并行扫描（rayon，64K 条目一个分块），结果再按需要排序，避免随机访问名字池；
- 子串查询在编译期构造一次 `memchr::memmem::Finder`，不再为每个条目重建；
- 常见通配符（`*.ext`、`prefix*`、`*substr*`）走 `ends_with`/`starts_with`/`memmem` 快路径；
- 全路径查询复用路径与折叠缓冲区，不再为每个条目分配 `Vec`。

## 索引与内存

| 指标 | 结果 |
|---|---:|
| 冷索引（扫描 + 建索引 + 写快照 + 挂 watch） | 2.7 s |
| 快照大小 | 25 MB |
| 快照加载（zstd 流式解码 + 重建父子链） | 0.46 s |
| 重建 12.4 万 inotify watch | 0.45 s |
| 守护进程 RSS（稳定） | 112–114 MB |
| 守护进程峰值 RSS（VmHWM） | 118–123 MB |

内存结构：

- 每条 `Entry` 固定 **48 字节**：size、mtime、created、accessed、name_off、fold_off、parent、first_child、next_sibling、name_len、kind、flags；
- 名字只存一份（不建去重哈希表）；折叠名仅在大小写确有差异时追加；
- 不再维护 `by_name` / `by_name_cs` 排序数组和 `DashMap`/`HashMap` 子项表，父目录通过 `first_child`/`next_sibling` 链式访问；
- 快照流式写入 zstd，不再构造整块 raw 缓冲；保存后调用 `malloc_trim(0)` 归还页。

## 增量更新与快照

- 文件创建 → 查询可见：80 ms 防抖窗口后即可见（人工验证，含 CLI 进程开销时约 100–200 ms）；
- 删除、改名、目录子树新增均走同一增量路径；
- 快照文件（`db.eldb`/`db.tmp`）、socket 等内部路径在扫描和 inotify 事件中均被排除，保存快照不会再触发索引更新；
- 只有 `generation` 变化时才按 `snapshot_interval_secs` 落盘；
- `cargo test --offline`：**29 个测试全部通过**（store 8、persist 3、query 10、incremental 5、auto_root 3）。

## 与 v0.1 的对比

| 指标 | v0.1 | v0.2 |
|---|---:|---:|
| 106 万条目 RSS | 441–468 MB | **112–114 MB** |
| 每条目内存 | 约 440 B | **约 105 B** |
| 快照 | 19 MB | 25 MB |
| 冷索引 | 约 30–40 s | **2.7 s** |
| 重启到可查询 | 约 5 s | **0.9 s** |
| 子串查询 | 约 180 ms | **8–12 ms** |
| 全路径查询 | 约 1.5 s | **60–65 ms** |

v0.2 去掉了可变 `Store` 与不可变 `IndexSnapshot` 两份索引、两套 56 字节元数据、名字的 4 份副本以及每条的哈希表/排序数组，改为单一 48 字节条目索引；查询改为并行扫描并按需排序。
