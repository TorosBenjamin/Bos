/// RX descriptor — hardware fills this when a packet arrives.
/// Must be exactly 16 bytes; layout is mandated by the e1000 spec.
#[repr(C)]
pub struct RxDesc {
    pub buf_addr: u64, // physical address of the receive buffer (set by software)
    pub length:   u16, // number of bytes written (set by hardware)
    pub checksum: u16,
    pub status:   u8,  // bit 0 = DD (descriptor done), bit 1 = EOP
    pub errors:   u8,
    pub special:  u16,
}

/// TX descriptor (legacy format) — software fills this to send a packet.
/// Must be exactly 16 bytes.
#[repr(C)]
pub struct TxDesc {
    pub buf_addr: u64, // physical address of the packet data
    pub length:   u16, // number of bytes to transmit
    pub cso:      u8,  // checksum offset (0 = unused)
    pub cmd:      u8,  // EOP | IFCS | RS — see TX_CMD_* constants
    pub status:   u8,  // bit 0 = DD set by hardware when transmit is done
    pub css:      u8,  // checksum start (0 = unused)
    pub special:  u16,
}

const _: () = assert!(core::mem::size_of::<RxDesc>() == 16);
const _: () = assert!(core::mem::size_of::<TxDesc>() == 16);
