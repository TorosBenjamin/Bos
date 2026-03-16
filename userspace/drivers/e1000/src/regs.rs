// e1000 (82540EM) MMIO register offsets
pub const CTRL:     u32 = 0x0000;
pub const _STATUS:  u32 = 0x0008;
pub const _EERD:    u32 = 0x0014;
pub const ICR:      u32 = 0x00C0;
pub const IMC:      u32 = 0x00D8;
pub const RCTL:     u32 = 0x0100;
pub const RDBAL:    u32 = 0x2800;
pub const RDBAH:    u32 = 0x2804;
pub const RDLEN:    u32 = 0x2808;
pub const RDH:      u32 = 0x2810;
pub const RDT:      u32 = 0x2818;
pub const TCTL:     u32 = 0x0400;
pub const TIPG:     u32 = 0x0410;
pub const TDBAL:    u32 = 0x3800;
pub const TDBAH:    u32 = 0x3804;
pub const TDLEN:    u32 = 0x3808;
pub const TDH:      u32 = 0x3810;
pub const TDT:      u32 = 0x3818;
pub const RAL0:     u32 = 0x5400;
pub const RAH0:     u32 = 0x5404;
pub const MTA_BASE: u32 = 0x5200; // 128 × u32 multicast table

// CTRL bits
pub const CTRL_SLU: u32 = 1 << 6;  // Set Link Up
pub const CTRL_RST: u32 = 1 << 26; // Software Reset

// RCTL bits
// EN=bit1, BAM=bit15, BSIZE=bits16-17 (00→2048), SECRC=bit26
pub const RCTL_EN:    u32 = 1 << 1;
pub const RCTL_BAM:   u32 = 1 << 15;
pub const RCTL_SECRC: u32 = 1 << 26;

// TCTL value: EN | PSP | CT=0x10 | COLD=0x40
pub const TCTL_VAL: u32 = (1 << 1) | (1 << 3) | (0x10 << 4) | (0x40 << 12);

// TIPG standard value for 802.3 (IPGT=10, IPGR1=8, IPGR2=6)
pub const TIPG_VAL: u32 = 10 | (8 << 10) | (6 << 20);

// TX descriptor CMD byte bits
pub const TX_CMD_EOP:  u8 = 1 << 0; // End of Packet
pub const TX_CMD_IFCS: u8 = 1 << 1; // Insert FCS/CRC
pub const TX_CMD_RS:   u8 = 1 << 3; // Report Status (sets DD on completion)

// Descriptor status bits (shared between RX and TX)
pub const DESC_DD: u8 = 1 << 0; // Descriptor Done

// IPC message type bytes
pub const MSG_TX_PACKET:  u8 = 1; // [type][len: u16 LE][data: len bytes]
pub const MSG_SUBSCRIBE:  u8 = 2; // [type][send_ep: u64 LE]
pub const MSG_GET_MAC:    u8 = 3; // [type][reply_ep: u64 LE]  → [mac: 6 bytes]
