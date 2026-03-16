# Drivers

## PS/2 Keyboard

The keyboard driver processes PS/2 Set 1 scancodes from port 0x60.

### Scancode processing

Each scancode byte is processed through a state machine:

- **0xE0 prefix**: marks the next scancode as an extended key (arrow keys, right modifiers, Home/End/PgUp/PgDn/Insert/Delete).
- **Bit 7**: if set, the key was released; if clear, it was pressed. The lower 7 bits are the key code.
- **Modifier tracking**: Shift, Ctrl, Alt, Super, and CapsLock state are tracked globally. CapsLock toggles on press only.

Scancodes are translated to `KeyEvent` structs containing the character (if printable), key type (arrow, function key, etc.), modifiers, and pressed/released state.

### Input buffer

A 64-entry ring buffer stores pending key events. When full, new events overwrite the oldest entry — for input events, stale data is less useful than fresh data. If userspace isn't consuming fast enough, it's better to drop old events than to drop the key the user just pressed.

### Wakeup

When a key event arrives, the driver wakes:
- A task blocked on `sys_read_key` (via `KEYBOARD_WAITER`)
- A task blocked on `sys_wait_for_event` watching keyboard events (via `KEYBOARD_EVENT_WAITER`)

Wakeup includes sending a reschedule IPI if the waiting task is on a different CPU.

## PS/2 Mouse

The mouse driver processes 3-byte PS/2 packets from port 0x60.

### Packet format

| Byte | Contents |
|------|----------|
| 0 | Status: buttons (bits 0-2), X/Y sign (bits 4-5), overflow (bits 6-7) |
| 1 | X movement (unsigned, sign-extended via bit 4 of status) |
| 2 | Y movement (unsigned, sign-extended via bit 5 of status) |

Packets with overflow bits set are discarded. Y movement is inverted (PS/2 positive = up, but screen coordinates positive = down).

### Synchronization

Bit 3 of the status byte is always set in a valid PS/2 packet. If a byte at position 0 doesn't have bit 3 set, the driver discards it and waits for a valid status byte. This re-synchronizes the 3-byte accumulator after data loss.

### Initialization

Mouse initialization enables the aux port, configures IRQ12, and sends the 0xF4 (enable streaming) command. The ACK byte (0xFA) is drained via polling before interrupts are enabled — if it weren't, the first IRQ12 would deliver 0xFA as data, permanently misaligning the packet accumulator.

### Buffer

Same 64-entry ring buffer design as the keyboard, with overwrite-oldest-on-full behavior.

## IDE/ATA Disk

The disk driver supports the primary IDE channel (master drive) with both DMA and PIO transfer modes.

### DMA (Bus Master DMA)

DMA is the preferred mode when available. The driver:

1. Detects the Bus Master interface by reading PCI BAR4 of the IDE controller.
2. Allocates a Physical Region Descriptor Table (PRDT) page and 32 data pages (128 KiB max transfer).
3. For a read: sets up PRDT entries pointing to data pages, configures the Bus Master registers (base address, direction, start bit), and issues the ATA READ DMA command.
4. The ATA interrupt (IRQ14) fires when the transfer completes.

DMA is faster because the disk controller transfers data directly to memory without CPU involvement. The CPU is free to run other tasks during the transfer.

### PIO (Programmed I/O)

When DMA is not available (no Bus Master interface), the driver falls back to PIO: the CPU reads/writes sector data 16 bits at a time via I/O ports 0x1F0-0x1F7. This is slower but universally supported.

### Addressing

The driver uses LBA28 (28-bit Logical Block Addressing), supporting drives up to ~137 GB. Drive identification is done via the ATA IDENTIFY command, which returns the total sector count.

## PCI

The PCI driver provides configuration space access for device discovery and driver initialization.

### Enumeration

Standard PCI bus scanning using I/O ports 0xCF8 (address) and 0xCFC (data). The driver can enumerate all devices by iterating over bus/device/function triplets, reading vendor and device IDs.

### Syscall interface

Three syscalls expose PCI to user tasks:

- `PciConfigRead(bus, device, function, offset, size)` — read 1/2/4 bytes from config space.
- `PciConfigWrite(bus, device, function, offset, size, value)` — write to config space.
- `MapPciBar(bus, device, function, bar_index)` — read a BAR address, map the MMIO region into the caller's address space, and return the virtual address.

This allows user-space drivers to directly access PCI devices without kernel-side driver code for each device type.
