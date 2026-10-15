// SPDX-License-Identifier: GPL-3.0-or-later
//
// Process lineage for osreflog: one ring-buffer record per new process (fork), program
// start (exec) and process exit. Threads are skipped (pid != tgid). Exec records carry the
// executable path, comm, uid, cgroup id and the first bytes of argv, read at exec time:
// most actors (sed -i, rpm scriptlets, systemctl) have exited before userspace sees their
// file events, so this is the only reliable moment to learn who they were.
//
// Kernel fields are declared below with CO-RE relocations, so the object loads on any kernel
// with BTF without a generated vmlinux.h. Tracepoint signatures checked on 7.2:
//   sched_process_fork(parent, child), sched_process_exec(task, old_pid, bprm),
//   sched_process_exit(task, group_dead) -- only the task is read, so older kernels work too.

#include <stdbool.h>
#include <linux/types.h>
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COMM_LEN 16
#define PATH_LEN 256
#define ARGS_LEN 256

enum kind { FORK = 1, EXEC = 2, EXIT = 3 };

struct kernfs_node {
	__u64 id;
} __attribute__((preserve_access_index));

struct cgroup {
	struct kernfs_node *kn;
} __attribute__((preserve_access_index));

struct css_set {
	struct cgroup *dfl_cgrp;
} __attribute__((preserve_access_index));

struct mm_struct {
	unsigned long arg_start;
	unsigned long arg_end;
} __attribute__((preserve_access_index));

struct task_struct {
	int pid;
	int tgid;
	struct task_struct *real_parent;
	struct css_set *cgroups;
	struct mm_struct *mm;
	char comm[COMM_LEN];
} __attribute__((preserve_access_index));

struct linux_binprm {
	const char *filename;
} __attribute__((preserve_access_index));

// Layout shared with src/lineage.rs (offsets asserted there and here).
struct event {
	__u8 kind;
	__u8 pad[3];
	__u32 pid;
	__u32 ppid;
	__u32 uid;
	__u64 ts_ns;
	__u64 cgroup;
	char comm[COMM_LEN];
	char filename[PATH_LEN];
	__u32 args_len;
	__u32 pad2;
	char args[ARGS_LEN];
};

_Static_assert(sizeof(struct event) == 568, "struct event layout changed");

struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(max_entries, 1 << 20);
} events SEC(".maps");

// Records lost because the ring buffer was full: userspace turns these into a gap.
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, __u64);
} drops SEC(".maps");

static __always_inline void count_drop(void)
{
	__u32 key = 0;
	__u64 *n = bpf_map_lookup_elem(&drops, &key);
	if (n)
		__sync_fetch_and_add(n, 1);
}

static __always_inline struct event *start(enum kind kind, struct task_struct *task)
{
	struct event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
	if (!e) {
		count_drop();
		return 0;
	}
	e->kind = kind;
	e->pid = BPF_CORE_READ(task, tgid);
	e->ppid = BPF_CORE_READ(task, real_parent, tgid);
	e->uid = (__u32)bpf_get_current_uid_gid();
	e->ts_ns = bpf_ktime_get_boot_ns();
	e->cgroup = BPF_CORE_READ(task, cgroups, dfl_cgrp, kn, id);
	BPF_CORE_READ_STR_INTO(&e->comm, task, comm);
	e->filename[0] = 0;
	e->args_len = 0;
	return e;
}

SEC("tp_btf/sched_process_fork")
int BPF_PROG(on_fork, struct task_struct *parent, struct task_struct *child)
{
	if (BPF_CORE_READ(child, pid) != BPF_CORE_READ(child, tgid))
		return 0;
	struct event *e = start(FORK, child);
	if (!e)
		return 0;
	// The child's real_parent may not be set up the way we read it yet; use the parent.
	e->ppid = BPF_CORE_READ(parent, tgid);
	bpf_ringbuf_submit(e, 0);
	return 0;
}

SEC("tp_btf/sched_process_exec")
int BPF_PROG(on_exec, struct task_struct *task, int old_pid, struct linux_binprm *bprm)
{
	struct event *e = start(EXEC, task);
	if (!e)
		return 0;
	bpf_probe_read_kernel_str(e->filename, sizeof(e->filename), BPF_CORE_READ(bprm, filename));
	unsigned long arg_start = BPF_CORE_READ(task, mm, arg_start);
	unsigned long arg_end = BPF_CORE_READ(task, mm, arg_end);
	unsigned long len = arg_end > arg_start ? arg_end - arg_start : 0;
	if (len > ARGS_LEN - 1)
		len = ARGS_LEN - 1;
	len &= ARGS_LEN - 1;
	if (len > 0 && bpf_probe_read_user(e->args, len, (const void *)arg_start) == 0)
		e->args_len = len;
	bpf_ringbuf_submit(e, 0);
	return 0;
}

SEC("tp_btf/sched_process_exit")
int BPF_PROG(on_exit, struct task_struct *task)
{
	if (BPF_CORE_READ(task, pid) != BPF_CORE_READ(task, tgid))
		return 0;
	struct event *e = start(EXIT, task);
	if (!e)
		return 0;
	bpf_ringbuf_submit(e, 0);
	return 0;
}

char LICENSE[] SEC("license") = "GPL";
