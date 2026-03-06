/// Initialize the Page Attribute Table (PAT) MSR for the current CPU.
///
/// Must be called on every logical processor (BSP + all APs) because IA32_PAT
/// is a per-CPU MSR — each core keeps its own copy.
///
/// # What this does
///
/// The x86 PAT MSR (0x277) maps 8 index slots to memory types. The slot is
/// chosen by three page-table bits: PAT_bit (bit 7 for 4 KB PTEs), PCD (bit 4),
/// and PWT (bit 3):
///
///   index = (PAT_bit << 2) | (PCD << 1) | PWT
///
/// Default PAT layout (Intel SDM):
///   [0] WB  [1] WT  [2] UC- [3] UC
///   [4] WB  [5] WT  [6] UC- [7] UC
///
/// We replace slot 1 (WT, selected by PWT=1 / `WRITE_THROUGH` flag) with WC
/// (Write-Combining). The framebuffer mapping in `sys_transfer_display` already
/// uses `PageTableFlags::WRITE_THROUGH`, so after this call it automatically
/// gets WC semantics — no page-flag changes needed.
///
/// Write-Combining coalesces CPU stores into burst transactions before sending
/// them to the memory bus, which is far more efficient than Write-Through for
/// bulk framebuffer updates.
pub fn init() {
    const IA32_PAT: u32 = 0x277;
    const WC: u64 = 0x01; // Write-Combining memory type encoding

    let mut pat = x86_64::registers::model_specific::Msr::new(IA32_PAT);
    unsafe {
        let current = pat.read();
        // PAT[1] lives in bits [15:8]. Clear the old value (WT = 0x04) and write WC.
        let new_val = (current & !0xFF00) | (WC << 8);
        pat.write(new_val);
    }
}
