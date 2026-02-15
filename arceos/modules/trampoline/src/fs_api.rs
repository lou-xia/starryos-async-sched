use executor::link::{create_link, FilePath};

pub async fn fs_init() {
    use alloc::format;
    use alloc::string::ToString;
    #[cfg(target_arch = "riscv64")]
    let libc_so = &"ld-musl-riscv64-sf.so.1";
    #[cfg(target_arch = "riscv64")]
    let libc_so2 = &"ld-musl-riscv64.so.1"; // 另一种名字的 libc.so，非 libc-test 测例库用

    #[cfg(target_arch = "x86_64")]
    let libc_so = &"ld-musl-x86_64-sf.so.1";
    #[cfg(target_arch = "x86_64")]
    let libc_so2 = &"ld-musl-x86_64.so.1"; // 另一种名字的 libc.so，非 libc-test 测例库用

    #[cfg(target_arch = "aarch64")]
    let libc_so = &"ld-musl-aarch64-sf.so.1";
    #[cfg(target_arch = "aarch64")]
    let libc_so2 = &"ld-musl-aarch64.so.1"; // 另一种名字的 libc.so，非 libc-test 测例库用

    create_link(
        &(FilePath::new(("/lib/".to_string() + libc_so).as_str())
            .await
            .unwrap()),
        &(FilePath::new("libc.so").await.unwrap()),
    )
    .await;
    create_link(
        &(FilePath::new(("/lib/".to_string() + libc_so2).as_str())
            .await
            .unwrap()),
        &(FilePath::new("libc.so").await.unwrap()),
    )
    .await;

    let tls_so = &"tls_get_new-dtv_dso.so";
    create_link(
        &(FilePath::new(("/lib/".to_string() + tls_so).as_str())
            .await
            .unwrap()),
        &(FilePath::new("tls_get_new-dtv_dso.so").await.unwrap()),
    )
    .await;

    // 接下来对 busybox 相关的指令建立软链接
    let busybox_arch = ["ls", "mkdir", "touch", "mv", "busybox", "sh", "which", "cp"];
    for arch in busybox_arch {
        let src_path = "/usr/sbin/".to_string() + arch;
        create_link(
            &(FilePath::new(src_path.as_str()).await.unwrap()),
            &(FilePath::new("busybox").await.unwrap()),
        )
        .await;
        let src_path = "/usr/bin/".to_string() + arch;
        create_link(
            &(FilePath::new(src_path.as_str()).await.unwrap()),
            &(FilePath::new("busybox").await.unwrap()),
        )
        .await;
        let src_path = "/bin/".to_string() + arch;
        create_link(
            &(FilePath::new(src_path.as_str()).await.unwrap()),
            &(FilePath::new("busybox").await.unwrap()),
        )
        .await;
    }
    create_link(
        &(FilePath::new("/bin/lmbench_all").await.unwrap()),
        &(FilePath::new("/lmbench_all").await.unwrap()),
    )
    .await;
    create_link(
        &(FilePath::new("/bin/iozone").await.unwrap()),
        &(FilePath::new("/iozone").await.unwrap()),
    )
    .await;

    #[cfg(target_arch = "x86_64")]
    {
        let libc_zlm = &"/lib/ld-linux-x86-64.so.2";
        create_link(
            &(FilePath::new(libc_zlm).await.unwrap()),
            &(FilePath::new("ld-linux-x86-64.so.2").await.unwrap()),
        )
        .await;

        create_link(
            &(FilePath::new("/lib/libssl.so.3").await.unwrap()),
            &(FilePath::new("libssl.so.3").await.unwrap()),
        )
        .await;

        create_link(
            &(FilePath::new("/lib/libcrypto.so.3").await.unwrap()),
            &(FilePath::new("libcrypto.so.3").await.unwrap()),
        )
        .await;

        create_link(
            &(FilePath::new("/lib/libstdc++.so.6").await.unwrap()),
            &(FilePath::new("libstdc++.so.6").await.unwrap()),
        )
        .await;

        create_link(
            &(FilePath::new("/lib/libm.so.6").await.unwrap()),
            &(FilePath::new("libm.so.6").await.unwrap()),
        )
        .await;

        create_link(
            &(FilePath::new("/lib/libgcc_s.so.1").await.unwrap()),
            &(FilePath::new("libgcc_s.so.1").await.unwrap()),
        )
        .await;

        create_link(
            &(FilePath::new("/lib/libc.so.6").await.unwrap()),
            &(FilePath::new("libc.so.6").await.unwrap()),
        )
        .await;
    }

    // let mem_file = axfs::api::lookup("/proc/meminfo").await.unwrap();
    // mem_file.write_at(0, meminfo().as_bytes()).unwrap();
    // let oom_file = axfs::api::lookup("/proc/sys/vm/overcommit_memory").await.unwrap();
    // oom_file.write_at(0, oominfo().as_bytes()).unwrap();
    // let fs_file = axfs::api::lookup("/proc/filesystems").await.unwrap();
    // fs_file.write_at(0, b"fat32\next4\n").unwrap();

    // let status_file = axfs::api::lookup("/proc/self/status").await.unwrap();
    // let status_info: &[u8] = &get_status_info(&axtask::current());
    // status_file.write_at(0, status_info).unwrap();

    // // create the file for the lmbench testcase
    // let _ = new_file("/lat_sig", &(FileFlags::CREATE | FileFlags::RDWR));

    // gcc相关的链接，可以在testcases/gcc/riscv64-linux-musl-native/lib目录下使用ls -al指令查看
    let src_dir = "riscv64-linux-musl-native/lib";
    create_link(
        &FilePath::new(format!("{}/ld-musl-riscv64.so.1", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new("/lib/libc.so").await.unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libatomic.so", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libatomic.so.1.2.0", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libatomic.so.1", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libatomic.so.1.2.0", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libgfortran.so", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libgfortran.so.5.0.0", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libgfortran.so.5", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libgfortran.so.5.0.0", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libgomp.so", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libgomp.so.1.0.0", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libgomp.so.1", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libgomp.so.1.0.0", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libssp.so", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libssp.so.0.0.0", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libssp.so.0", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libssp.so.0.0.0", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libstdc++.so", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libstdc++.so.6.0.29", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new(format!("{}/libstdc++.so.6", src_dir).as_str())
            .await
            .unwrap(),
        &FilePath::new(format!("{}/libstdc++.so.6.0.29", src_dir).as_str())
            .await
            .unwrap(),
    )
    .await;
    create_link(
        &FilePath::new("/lib/libgcc_s.so.1").await.unwrap(),
        &FilePath::new("/libgcc_s.so.1").await.unwrap(),
    )
    .await;
}

// fn meminfo() -> &'static str {
//     "MemTotal:       32246488 kB
// MemFree:         5239804 kB
// MemAvailable:   10106000 kB
// Buffers:          235604 kB
// Cached:          5204940 kB
// SwapCached:            0 kB
// Active:         17890456 kB
// Inactive:        2119348 kB
// Active(anon):   14891328 kB
// Inactive(anon):        0 kB
// Active(file):    2999128 kB
// Inactive(file):  2119348 kB
// Unevictable:         144 kB
// Mlocked:             144 kB
// SwapTotal:       8388604 kB
// SwapFree:        8388604 kB
// Zswap:                 0 kB
// Zswapped:              0 kB
// Dirty:               784 kB
// Writeback:             0 kB
// AnonPages:      14560300 kB
// Mapped:          2108592 kB
// Shmem:            323608 kB
// KReclaimable:     205804 kB
// Slab:            1539752 kB
// SReclaimable:     205804 kB
// SUnreclaim:      1333948 kB
// KernelStack:      630704 kB
// PageTables:      2007248 kB
// SecPageTables:         0 kB
// NFS_Unstable:          0 kB
// Bounce:                0 kB
// WritebackTmp:          0 kB
// CommitLimit:    24511848 kB
// Committed_AS:   42466972 kB
// VmallocTotal:   34359738367 kB
// VmallocUsed:      762644 kB
// VmallocChunk:          0 kB
// Percpu:            35776 kB
// HardwareCorrupted:     0 kB
// AnonHugePages:     79872 kB
// ShmemHugePages:        0 kB
// ShmemPmdMapped:        0 kB
// FileHugePages:         0 kB
// FilePmdMapped:         0 kB
// Unaccepted:            0 kB
// HugePages_Total:       0
// HugePages_Free:        0
// HugePages_Rsvd:        0
// HugePages_Surp:        0
// Hugepagesize:       2048 kB
// Hugetlb:               0 kB
// DirectMap4k:     6500036 kB
// DirectMap2M:    23283712 kB
// DirectMap1G:     3145728 kB"
// }

// // TODO: Implement the real content of overcommit_memory
// fn oominfo() -> &'static str {
//     "0"
// }

// fn get_status_info(task: &axtask::CurrentTask) -> Vec<u8> {
//     let name = task.name().as_bytes();
//     let id = task.id().as_u64().to_string();
//     let status_vec = [name, b"\n", id.as_bytes(), b"\n256\n"].concat();
//     status_vec
// }
