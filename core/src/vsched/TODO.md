# vsched2 集成 StarryOS — 问题跟踪

## 当前状态

| 项目 | 状态 |
|------|------|
| 初始化流程 | ✅ |
| "Welcome to Starry OS!" 输出 | ✅ |
| handle_page_fault n==0 时 PTE 检查 | ✅ |
| ktask_schedule !pop 时同步 priority | ✅ |
| AddrSpace.vdso_base per-process | ✅ |
| clone 用 map_so 重建 vDSO | ✅ |
| vDSO reserved gap 补齐 (mmap 防护) | ✅ |
| musl `__ofl_head` 毒指针 (0xEB230) | ✅ |
| execve `process_reinit` 复用 pid | ✅ |
| yield trampoline sepc/sstatus/SPP 补全 | ✅ |
| 全 syscall save/restore CURRENT_TASK | ✅ |
| exit_group 标记 VschedTaskImpl Exited | ✅ |
| LazyInit<AxRunQueue> panic 根除 | ✅ `init_run_queue_empty` + 4 条 guard |
| **pid=1 wait4 死循环 (子进程 zombie 标记)** | ❌ **新阻塞** |

---

## 🔴 P0 — pid=1 wait4 死循环 *(当前阻塞)*

### 现象

```
yield_stub (trap_handler, pid=0)
into_user_ctx (busybox, pid=1)  sepc=0xffffffc08037f92e  ← 全部内核寄存器
循环...
```

### 机制

pid=1 在 `wait4` → `block_on(vsched2 path)` 中：
1. poll future → 子进程未变成 zombie → `Poll::Pending`
2. yield → kscheduler → 重新选中 pid=1
3. 进入 S-mode (SPP=1) 继续 block_on 循环
4. 再次 poll → 仍然是 Pending → yield → 死循环

### 根因推测

`check_children` 中 `child.is_zombie()` 永不返回 true。pid=2 的 exit 流程中 `process.exit()` 可能未正确设置 zombie 标志，或 `exit_thread` 的返回值处理有问题。pid=1 永远等不到子进程变成 zombie，在 yield→poll→yield 中无限循环。

### 关系

与 `AxRunQueue` panic 修复**间接相关**——panic 修好后 pid=1 能无限循环而不是 crash，暴露了 exit/zombie 标记的预存问题。

---

## 📈 毒指针 0xeb230 数据流图 (已解决)

```
load_user_app: map_so → vDSO gap 未填满
    ↓
musl mmap(0, ...): find_free_area(0) → 从 vDSO gap 分配匿名页 → FILE* at 0xEB230
    ↓
fork → clone: unmap(vvar..vdso) → 删除 mmap 匿名页 → extension 填回全零页
    ↓
pid=2 __stdio_exit: s0=0xEB230 → *(s0+12)=0 → free(0) → crash
```
修复: vDSO reserved gap 补齐 → mmap 不再从 vDSO 区分配。

---

## ⚠️ 已知遗留问题

| 问题 | 状态 |
|------|------|
| execve `process_init` 分配新 pid 导致旧 slot 泄漏 | ⚠️ 待处理 |
| exit 缺少 `process_drop` → PROCESS_INFO 表泄漏 | ⚠️ 待处理 |
| vDSO 物理页在 `uspace.clear()` 后未释放 | ⚠️ 待处理 |
| `do_exit` 中 `clear_child_tid` 继承自父进程 | ⚠️ 待处理 |
