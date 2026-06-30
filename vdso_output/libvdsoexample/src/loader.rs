use alloc::string::ToString;
use core::str::from_utf8;
use crate_interface::{call_interface, def_interface};
use include_bytes_aligned::include_bytes_aligned;
pub use page_table_entry::MappingFlags;
use vdso_example::VvarData;
use xmas_elf::program::SegmentData;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};
use lazyinit::LazyInit;

/// 因为不同系统中代表物理页的类型（假设为`PhysPage`）不同，
/// 物理页类型可能使用RAII管理（在物理页对象释放时释放实际物理页），
/// 而接口中很难支持泛型，
/// 所以，本库以`*const ManuallyDrop<PhysPage>`的形式管理物理页，
/// 并转化为usize以取消泛型属性并方便作为全局变量。
/// 
/// （`ManuallyDrop`只是为了强调指针指向的物理页不会被自动释放，不一定要求转化的类型中一定带有`ManuallyDrop`。
/// 例如，如果os中使用`Arc<PhysPage>`管理物理页，那么使用`Arc::into_raw()`得到的`*const PhysPage`就可以作为`PhysPagePtr`。）
/// 
/// `PhysPagePtr`的生命周期如下图：
/// 
/// `MemIf::ppage_alloc`将`PhysPage`转化为`PhysPagePtr` ➡ `MemIf::ppage_clone`复制 ➡ 存储在库中
/// 
///                                  ⬇                                                  ⬇
/// 
///                                  ⬇                                       `MemIf::ppage_clone`复制
/// 
///                                  ⬇                                                  ⬇
/// 
///                                  ➡          ➡          `MemIf::map`将`PhysPagePtr`重新转化为`PhysPage`，并加入页表
/// 
/// 保证每个指针的生命周期从`MemIf::ppage_alloc`开始到`MemIf::map`结束，
/// 且每个指针只会`map`一次，`map`之后即不再使用该指针。
/// 
/// 为了实现物理页的共享，内核的vdso加载到的物理页指针需要`clone`后在本库中暂存一份。
/// 下次加载共享的物理页后，将暂存的指针再`clone`一次，并对`clone`后的指针调用`map`。
/// 这样保证了库中暂存的指针仍有效。
/// 虽然内核的物理页指针会被暂存一份，导致无法释放，但在内核加载的vdso页面本来就在内核关闭时才能释放，
/// 因此没有问题。
/// 
/// 加载用户vdso时新分配的物理页则没有`clone`和暂存的过程，在`alloc`后即调用`map`。
/// 因此不会影响`PhysPage`的生命周期管理。
pub type PhysPagePtr = usize;

/// 加载和映射vDSO使用的接口。
///
/// 实现了这些接口后，内核可以通过以下方式实现vDSO模块的初始化：
///
/// - 内核态初始化（以下步骤已封装在`map_and_init`函数中）：
///     1. 在内核调用`map_so`（需保证是首次调用），加载和重定位so文件
///     `map_so`的行为：无论共享数据、代码还是私有数据，均会分配物理页并加载。
///     2. 在内核调用`init_vdso_vtable`，初始化内核空间中的`VDSO_VTABLE`。
/// - 用户态初始化：
///     1. 在内核调用`map_so`（需保证是后续调用），加载和重定位so文件。
///     `map_so`的行为：对于共享数据和代码会映射到已分配的物理页；对于私有数据会重新分配物理页并加载。
///     2. 在用户态调用`init_vdso_vtable`，初始化用户空间中的`VDSO_VTABLE`。
#[def_interface]
pub trait MemIf {
    /// 在地址空间中分配用于vDSO和vVAR的虚存区域（不需同时分配物理页面），返回指向首地址的指针。
    /// 
    /// 保证size为build_vdso传入的config.page_size的整数倍。
    /// 要求返回的地址也为config.page_size的整数倍。
    fn valloc(vspace: usize, size: usize) -> *mut u8;

    /// 分配多块用于vDSO和vVAR的连续物理页，返回`PhysPagePtr`。
    /// 
    /// 保证size为build_vdso传入的config.page_size的整数倍。
    ///
    /// 若需要实现vDSO和vVAR在多地址空间的共享，则需要在分配时使这块空间可被共享（即，可被多次`map`）。
    fn ppage_alloc(size: usize) -> PhysPagePtr;

    /// 从`alloc`返回的虚存区域中，映射其中一块到某个物理页面并设置权限。
    /// 
    /// 被映射的物理页面可能和其它地址空间共享，也可能由这个地址空间独占。
    /// 
    /// 保证vaddr对齐到build_vdso传入的config.page_size；len为config.page_size的整数倍。
    ///
    /// `flags`可能包含：READ、WRITE、EXECUTE、USER。
    fn map(vspace: usize, vaddr: *mut u8, ppage: PhysPagePtr, size: usize, flags: MappingFlags);

    /// 重新设置已映射好的，虚拟首地址为`vspace`区域的权限。
    /// 
    /// 保证vaddr对齐到build_vdso传入的config.page_size。
    fn change_protect(vspace: usize, vaddr: *mut u8, size: usize, flags: MappingFlags);

    /// 获取`vspace`空间中`vaddr`地址对应的内核虚拟地址。
    /// （也就是当前代码可以直接访问的地址）
    fn get_kernel_vaddr(vspace: usize, vaddr: *mut u8) -> *mut u8;

    /// 复制物理页指针，复制前后指向同一块物理页。复制后，参数和返回值对应的两个指针均需可用。
    /// 
    /// 如果物理页使用RAII管理，则需调用其`clone`方法。
    /// 
    /// 如果物理页不使用RAII管理，则可以直接返回参数。
    fn ppage_clone(ppage: PhysPagePtr) -> PhysPagePtr;
}

const PAGES_SIZE: usize = 4096;
const VDSO: &[u8] = include_bytes_aligned!(8, "../../libvdsoexample.so");
const VDSO_SIZE: usize = ((VDSO.len() + PAGES_SIZE - 1) & (!(PAGES_SIZE - 1))) + PAGES_SIZE; // 额外加了一页，用于bss段等未出现在文件中的段
const VVAR_SIZE: usize = (core::mem::size_of::<VvarData>() + PAGES_SIZE - 1) & (!(PAGES_SIZE - 1));

/// 内核虚拟地址、内核物理页、大小、flags
static KERNEL_VDSO_REGIONS: LazyInit<Vec<(usize, PhysPagePtr, usize, MappingFlags)>> = LazyInit::new();

/// - 第一次调用：加载并映射vdso。
/// - 后续调用：将已加载的vdso映射到另一个地址空间。
/// 
/// 该函数的返回值为vDSO和vVAR的映射区域的信息，元组的四项依次为用户虚拟地址、内核虚拟地址、大小和访问权限。vDSO首地址为第二个映射区域的首地址。
pub fn map_so(vspace: usize) -> *mut u8 {
    let vbase = call_interface!(MemIf::valloc(vspace, VVAR_SIZE + VDSO_SIZE));
    let mut regions = Vec::new();

    // vVAR初始化
    #[cfg(feature = "log")]
    log::info!("mapping vVAR...");
    let vaddr = vbase;
    // ppage用于映射
    // ppage_store用于存储在KERNEL_VDSO_REGIONS中（只有首次调用时有意义）
    let (ppage, ppage_store) = if !KERNEL_VDSO_REGIONS.is_inited() {
        // 首次调用，分配物理页并加载vVAR
        let ppage = call_interface!(MemIf::ppage_alloc(VVAR_SIZE));
        let ppage_clone = call_interface!(MemIf::ppage_clone(ppage));
        (ppage, ppage_clone)
    } else {
        // 后续调用，映射已加载的vVAR
        let origin_ppage = KERNEL_VDSO_REGIONS.get().unwrap()[0].1;
        let ppage = call_interface!(MemIf::ppage_clone(origin_ppage));
        (ppage, ppage)
    };
    let flags = if !KERNEL_VDSO_REGIONS.is_inited() {
        // 首次调用，内核空间的vVAR不设置USER
        MappingFlags::READ | MappingFlags::WRITE
    } else {
        // 后续调用，用户空间的vVAR设置USER
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER
    };
    #[cfg(feature = "log")]
    log::info!(
        "map: vspace: 0x{:016x}, vaddr: 0x{:016x}, ppage_struct_ptr: 0x{:016x}, size: 0x{:x} {:?}",
        vspace,
        vaddr as usize,
        ppage,
        VVAR_SIZE,
        flags
    );
    call_interface!(MemIf::map(vspace, vaddr, ppage, VVAR_SIZE, flags));
    // 初始化vvar，只在首次调用时写入数据，后续调用时内核加载的vVAR页面已经包含了正确的数据。
    // 只在首次调用时，存储region信息
    if !KERNEL_VDSO_REGIONS.is_inited() {
        unsafe { (vaddr as *mut VvarData).write(VvarData::default()) };
        regions.push((vaddr as usize, ppage_store, VVAR_SIZE, flags));
    }

    // vDSO初始化
    #[cfg(feature = "log")]
    log::info!("mapping vDSO...");
    let vdso_elf = xmas_elf::ElfFile::new(VDSO).expect("Error parsing app ELF file.");
    if let Some(interp) = vdso_elf
        .program_iter()
        .find(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Interp))
    {
        let interp = match interp.get_data(&vdso_elf) {
            Ok(SegmentData::Undefined(data)) => data,
            _ => panic!("Invalid data in Interp Elf Program Header"),
        };

        let interp_path = from_utf8(interp).expect("Interpreter path isn't valid UTF-8");
        // remove trailing '\0'
        let _interp_path = interp_path.trim_matches(char::from(0)).to_string();
        #[cfg(feature = "log")]
        log::debug!("Interpreter path: {:?}", _interp_path);
    }
    let elf_base_addr = Some((vbase as usize) + VVAR_SIZE);
    let segments = elf_parser::get_elf_segments(&vdso_elf, elf_base_addr);
    let relocate_pairs = elf_parser::get_relocate_pairs(&vdso_elf, elf_base_addr);
    let mut index = 1;
    for segment in segments {
        if segment.size == 0 {
            #[cfg(feature = "log")]
            log::warn!(
                "Segment with size 0 found, skipping: {:?}, {:#x}, {:?}",
                segment.vaddr,
                segment.size,
                segment.flags
            );
            continue;
        }
        #[cfg(feature = "log")]
        log::debug!(
            "{:?}, {:#x}, {:?}",
            segment.vaddr,
            segment.size,
            segment.flags
        );

        assert!(segment.vaddr.as_usize() & (PAGES_SIZE - 1) == 0);
        let size = (segment.size + PAGES_SIZE - 1) & (!(PAGES_SIZE - 1));
        let vaddr = segment.vaddr.as_mut_ptr();
        let (ppage, ppage_store) = if !KERNEL_VDSO_REGIONS.is_inited() {
            // 首次调用，分配物理页并加载vDSO
            let ppage = call_interface!(MemIf::ppage_alloc(size));
            let ppage_clone = call_interface!(MemIf::ppage_clone(ppage));
            (ppage, ppage_clone)
        } else {
            // 后续调用
            if segment.flags.contains(MappingFlags::EXECUTE) {
                // 代码段，使用已加载的vDSO
                let origin_ppage = KERNEL_VDSO_REGIONS.get().unwrap()[index].1;
                let ppage = call_interface!(MemIf::ppage_clone(origin_ppage));
                (ppage, ppage)
            } else {
                // 数据段，重新分配物理页，且后续需要加载和重定位
                let ppage = call_interface!(MemIf::ppage_alloc(size));
                (ppage, ppage)
            }
        };
        let flags = if !KERNEL_VDSO_REGIONS.is_inited() {
            // 首次调用，内核空间的vDSO不设置USER
            segment.flags & !MappingFlags::USER
        } else {
            // 后续调用，用户空间的vDSO设置USER
            segment.flags | MappingFlags::USER
        };
        // 首先需以WRITE和!USER权限映射，以便加载和重定位；加载和重定位完成后再设置为最终权限。
        let flags_with_write = flags | MappingFlags::WRITE & !MappingFlags::USER;
        #[cfg(feature = "log")]
        log::info!(
            "map: vspace: 0x{:016x}, vaddr: 0x{:016x}, ppage_struct_ptr: 0x{:016x}, size: 0x{:x} {:?}",
            vspace,
            vaddr as usize,
            ppage,
            size,
            flags_with_write
        );
        call_interface!(MemIf::map(vspace, vaddr, ppage, size, flags_with_write));
        if !KERNEL_VDSO_REGIONS.is_inited() || !segment.flags.contains(MappingFlags::EXECUTE) {
            // “首次调用”或“后续调用的数据段”，加载和重定位vDSO
            // 因为在“后续调用的数据段”情况下，虚拟地址不一定能直接访问，因此需要转化。
            if let Some(data) = segment.data {
                assert!(data.len() <= size);
                let src = data.as_ptr();
                let dst = call_interface!(MemIf::get_kernel_vaddr(vspace, vaddr));
                let count = data.len();
                unsafe {
                    core::ptr::copy_nonoverlapping(src, dst, count);
                    if size > count {
                        core::ptr::write_bytes(dst.add(count), 0, size - count);
                    }
                }
            } else {
                unsafe { core::ptr::write_bytes(vaddr, 0, size) };
            }
            for relocate_pair in &relocate_pairs {
                let relo_src: usize = relocate_pair.src.into();
                let relo_dst: usize = relocate_pair.dst.into();
                let count = relocate_pair.count;
                if segment.vaddr.as_usize() <= relo_dst
                    && relo_dst < segment.vaddr.as_usize() + size
                {
                    let relo_kdst =
                        call_interface!(MemIf::get_kernel_vaddr(vspace, relo_dst as *mut u8));
                    #[cfg(feature = "log")]
                    log::info!(
                        "Relocate: src: 0x{:x}, udst: 0x{:x}, kdst: 0x{:x}, count: {}",
                        relo_src,
                        relo_dst,
                        relo_kdst as usize,
                        count
                    );
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            relo_src.to_ne_bytes().as_ptr(),
                            relo_kdst,
                            count,
                        )
                    }
                }
            }
        } else {
            // 后续调用的代码段，确认代码段没有重定位
            for relocate_pair in &relocate_pairs {
                let relo_dst: usize = relocate_pair.dst.into();
                if vaddr as usize <= relo_dst && relo_dst < vaddr as usize + size {
                    panic!("Relocate pair found in text section!");
                }
            }
        }
        if flags != flags_with_write {
            #[cfg(feature = "log")]
            log::info!(
                "change_protect: vspace: 0x{:016x}, vaddr: 0x{:016x}, size: 0x{:x}, flags: {:?}",
                vspace,
                vaddr as usize,
                size,
                flags
            );
            call_interface!(MemIf::change_protect(vspace, vaddr, size, flags));
        }
        if !KERNEL_VDSO_REGIONS.is_inited() {
            regions.push((vaddr as usize, ppage_store, size, flags));
        }
        index += 1;
    }

    #[cfg(feature = "log")]
    log::info!("mapping complete!");

    if !KERNEL_VDSO_REGIONS.is_inited() {
        KERNEL_VDSO_REGIONS.init_once(regions);
    }

    ((vbase as usize) + VVAR_SIZE) as _
}
