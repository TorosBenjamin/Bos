# Timekeeping

Bos uses a layered timing architecture: the PIT provides a reference for one-time calibration, the TSC provides fast elapsed-time measurement, and the LAPIC timer drives periodic scheduling interrupts.

## TSC (Time Stamp Counter)

The TSC is the primary time source. It's a per-CPU counter that increments at a fixed rate (on modern CPUs with invariant TSC). Reading the TSC is a single instruction (`rdtsc` or `rdtscp`) with no I/O — much faster than reading a hardware timer.

### Calibration

The TSC's tick rate isn't known at boot. Bos calibrates it against the PIT (which has a known frequency of 1.193182 MHz):

1. Configure PIT channel 2 in one-shot mode for 10 ms.
2. Read TSC before and after the PIT countdown.
3. Compute `ticks_per_ms = (tsc_after - tsc_before) / 10`.

This is done once during BSP boot. All subsequent time calculations use this value.

### RDTSCP vs RDTSC

The kernel detects RDTSCP support via CPUID. RDTSCP is preferred because it's a serializing instruction — it guarantees all prior instructions complete before reading the counter. Without serialization (`rdtsc` alone), out-of-order execution can cause the read to happen before preceding instructions complete, giving inaccurate measurements. As a fallback, the kernel uses `lfence; rdtsc` which achieves the same serialization effect.

### Wall-clock time

During boot, the RTC (CMOS real-time clock) is read to get the current date/time, which is converted to Unix epoch nanoseconds and stored alongside the corresponding TSC value:

```
current_time_ns = boot_unix_ns + (current_tsc - boot_tsc) * 1_000_000 / ticks_per_ms
```

The multiplication is split to avoid overflow: `(elapsed_tsc / ticks_per_ms) * 1_000_000 + remainder * 1_000_000 / ticks_per_ms`.

`sys_get_time_ns` exposes this to user tasks.

## LAPIC timer

Each CPU's local APIC has a timer that drives preemptive scheduling. Bos uses **TSC-deadline mode**: instead of counting down from a value, the timer fires when the TSC reaches a programmed deadline.

On each timer interrupt, `on_timer_tick()`:

1. Handles the scheduling decision (context switch, run queue management).
2. Checks for timed event waiters (tasks sleeping via `sys_sleep_ms` or `sys_wait_for_event` with a timeout).
3. Programs the next deadline: `current_tsc + ticks_per_ms` (1 ms interval).

TSC-deadline mode is used instead of periodic mode because it's simpler (one MSR write per tick) and aligns naturally with TSC-based time calculations. On x2apic, the deadline is set via the `IA32_TSC_DEADLINE` MSR — no MMIO contention between CPUs.

## PIT (Programmable Interval Timer)

The PIT (Intel 8253/8254) is only used during boot for TSC calibration. After that, it's superseded by the LAPIC timer.

Channel 2 (speaker/tone output) is used because it doesn't conflict with other PIT uses and its completion can be detected by polling the OUT pin via port 0x61.

## RTC (Real-Time Clock)

The RTC is a battery-backed clock in the CMOS that maintains wall-clock time across reboots. It's read once during boot to anchor the TSC to real time. The kernel reads hours, minutes, seconds, day, month, and year from CMOS registers, waits for the update-not-in-progress flag, and converts to Unix nanoseconds.

After boot, the RTC is not accessed again — all time queries use the TSC.
