//! 64-bit long mode CPU state setup for direct kernel boot.
//!
//! Sets up identity-mapped page tables, a minimal GDT, and configures
//! segment registers / control registers for 64-bit long mode entry.

use nova_kvm::kvm_bindings::{KvmRegs, KvmSegment, KvmSregs};
use nova_mem::{GuestAddress, GuestMemoryMmap};

use crate::layout;

// x86_64 control register bits.
const CR0_PE: u64 = 1 << 0; // Protected Mode Enable
const CR0_PG: u64 = 1 << 31; // Paging

const CR4_PAE: u64 = 1 << 5; // Physical Address Extension

const EFER_LME: u64 = 1 << 8; // Long Mode Enable
const EFER_LMA: u64 = 1 << 10; // Long Mode Active
const EFER_SCE: u64 = 1 << 0; // System Call Extensions

// Page table entry flags.
const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_PS: u64 = 1 << 7; // Page Size (2 MiB huge page)

// GDT entry constants.
const GDT_ENTRY_SIZE: usize = 8;

/// Write identity-mapped page tables for 64-bit long mode.
///
/// Creates PML4 → PDPT → PD with 2 MiB huge pages covering 0–1 GiB.
/// Tables are written at the addresses defined in `layout`.
pub fn setup_long_mode_page_tables(mem: &GuestMemoryMmap) -> crate::Result<()> {
    // PML4[0] → PDPT
    let pml4_entry: u64 = layout::PDPT_ADDR | PTE_PRESENT | PTE_WRITABLE;
    mem.write_obj(GuestAddress::new(layout::PAGE_TABLE_ADDR), &pml4_entry)?;

    // PDPT[0] → PD
    let pdpt_entry: u64 = layout::PD_ADDR | PTE_PRESENT | PTE_WRITABLE;
    mem.write_obj(GuestAddress::new(layout::PDPT_ADDR), &pdpt_entry)?;

    // PD: 512 entries × 2 MiB = 1 GiB identity mapped
    for i in 0u64..512 {
        let pd_entry: u64 = (i * 0x20_0000) | PTE_PRESENT | PTE_WRITABLE | PTE_PS;
        let addr = layout::PD_ADDR + i * 8;
        mem.write_obj(GuestAddress::new(addr), &pd_entry)?;
    }

    tracing::debug!(
        pml4 = format!("{:#x}", layout::PAGE_TABLE_ADDR),
        pdpt = format!("{:#x}", layout::PDPT_ADDR),
        pd = format!("{:#x}", layout::PD_ADDR),
        "wrote 64-bit identity-mapped page tables (1 GiB)"
    );

    Ok(())
}

/// Write a minimal 64-bit GDT to guest memory.
///
/// Layout at `GDT_ADDR`:
///   0x00: null descriptor
///   0x08: 64-bit code segment (selector 0x08)
///   0x10: 64-bit data segment (selector 0x10)
pub fn setup_gdt(mem: &GuestMemoryMmap) -> crate::Result<()> {
    let gdt: [u64; 3] = [
        0,                     // null
        0x00AF_9A00_0000_FFFF, // code64: base=0, limit=0xFFFFF, P=1, DPL=0, S=1, type=0xA (exec/read), L=1, G=1
        0x00CF_9200_0000_FFFF, // data64: base=0, limit=0xFFFFF, P=1, DPL=0, S=1, type=0x2 (read/write), DB=1, G=1
    ];

    for (i, entry) in gdt.iter().enumerate() {
        let addr = layout::GDT_ADDR + (i * GDT_ENTRY_SIZE) as u64;
        mem.write_obj(GuestAddress::new(addr), entry)?;
    }

    tracing::debug!(
        addr = format!("{:#x}", layout::GDT_ADDR),
        "wrote 64-bit GDT"
    );

    Ok(())
}

/// Configure special registers for 64-bit long mode.
///
/// Sets CR0 (PE+PG), CR3 (page table root), CR4 (PAE), EFER (LME+LMA+SCE),
/// GDT pointer, and all segment registers with proper 64-bit attributes.
pub fn configure_64bit_sregs(sregs: &mut KvmSregs) {
    // Control registers.
    sregs.cr0 = CR0_PE | CR0_PG;
    sregs.cr3 = layout::PAGE_TABLE_ADDR;
    sregs.cr4 = CR4_PAE;
    sregs.efer = EFER_LME | EFER_LMA | EFER_SCE;

    // GDT register.
    sregs.gdt.base = layout::GDT_ADDR;
    sregs.gdt.limit = (3 * GDT_ENTRY_SIZE - 1) as u16;

    // 64-bit code segment (selector 0x08).
    sregs.cs = KvmSegment {
        base: 0,
        limit: 0xFFFF_FFFF,
        selector: 0x08,
        type_: 11, // execute/read, accessed
        present: 1,
        dpl: 0,
        db: 0, // must be 0 for 64-bit
        s: 1,
        l: 1, // 64-bit mode
        g: 1,
        avl: 0,
        unusable: 0,
        padding: 0,
    };

    // Data segments (selector 0x10).
    let data_seg = KvmSegment {
        base: 0,
        limit: 0xFFFF_FFFF,
        selector: 0x10,
        type_: 3, // read/write, accessed
        present: 1,
        dpl: 0,
        db: 1,
        s: 1,
        l: 0,
        g: 1,
        avl: 0,
        unusable: 0,
        padding: 0,
    };

    sregs.ds = data_seg;
    sregs.es = data_seg;
    sregs.ss = data_seg;
    sregs.fs = data_seg;
    sregs.gs = data_seg;
}

/// Configure general-purpose registers for 64-bit kernel entry.
pub fn configure_64bit_regs(regs: &mut KvmRegs, entry: u64) {
    regs.rip = entry;
    regs.rsp = layout::BOOT_STACK_ADDR;
    regs.rflags = 0x2; // bit 1 must always be set
}
