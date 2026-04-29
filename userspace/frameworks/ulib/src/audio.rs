//! Audio client helpers for the Bos sound server.
//!
//! Usage:
//! ```no_run
//! let ep = ulib::audio::connect();  // poll until sound_server is up
//! ulib::audio::play_pcm(ep, &samples); // send a chunk of s16le PCM
//! ulib::audio::set_volume(ep, 80);   // 0 = max, 100 = mute
//! ```

use crate::sys_lookup_service;
use kernel_api_types::audio::{AUDIO_MSG_PLAY_CHUNK, AUDIO_MSG_SET_VOLUME};
use kernel_api_types::SVC_ERR_NOT_FOUND;

/// Look up the audio server, polling up to ~500 yields.
/// Returns the send endpoint or `SVC_ERR_NOT_FOUND`.
pub fn connect() -> u64 {
    let mut ep = SVC_ERR_NOT_FOUND;
    for _ in 0..500u32 {
        ep = sys_lookup_service(b"audio");
        if ep != SVC_ERR_NOT_FOUND {
            return ep;
        }
        crate::sys_yield();
    }
    ep
}

/// Send `samples` (signed 16-bit LE, interleaved L/R for stereo) to the audio server.
///
/// Allocates a shared buffer, copies the samples into it, and sends a `PLAY_CHUNK` message.
/// The buffer is destroyed by the server after it has consumed the data.
///
/// `sample_rate` is typically 44100; `channels` is 1 (mono) or 2 (stereo).
pub fn play_pcm(audio_ep: u64, samples: &[i16], sample_rate: u32, channels: u8) {
    if audio_ep == SVC_ERR_NOT_FOUND || samples.is_empty() {
        return;
    }

    let byte_len = (samples.len() * 2) as u64;
    let (buf_id, ptr) = crate::sys_create_shared_buf(byte_len);
    if ptr.is_null() {
        return;
    }

    // Copy samples into the shared buffer.
    unsafe {
        core::ptr::copy_nonoverlapping(samples.as_ptr(), ptr as *mut i16, samples.len());
    }

    // Build the PLAY_CHUNK message on the stack.
    // Layout: [type:u8][buf_id:u64 LE][n_samples:u32 LE][channels:u8][sample_rate:u32 LE]
    let mut msg = [0u8; 18];
    msg[0] = AUDIO_MSG_PLAY_CHUNK;
    msg[1..9].copy_from_slice(&buf_id.to_le_bytes());
    msg[9..13].copy_from_slice(&(samples.len() as u32).to_le_bytes());
    msg[13] = channels;
    msg[14..18].copy_from_slice(&sample_rate.to_le_bytes());

    // Unmap our view — the server maps its own.
    crate::sys_munmap(ptr, byte_len);

    crate::sys_channel_send(audio_ep, &msg);
}

/// Set the master volume. `vol` = 0 means maximum loudness, 100 means mute.
pub fn set_volume(audio_ep: u64, vol: u8) {
    if audio_ep == SVC_ERR_NOT_FOUND {
        return;
    }
    let msg = [AUDIO_MSG_SET_VOLUME, vol];
    crate::sys_channel_send(audio_ep, &msg);
}
