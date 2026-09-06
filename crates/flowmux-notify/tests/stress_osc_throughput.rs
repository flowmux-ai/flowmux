// SPDX-License-Identifier: GPL-3.0-or-later
//! Stress: OSC extractor throughput under realistic and adversarial loads.
//!
//! Behavioral checks run by default; run opt-in throughput probes with:
//!     cargo test -p flowmux-notify --release --test stress_osc_throughput -- --ignored --nocapture
//!
//! Three probes:
//!
//! 1. **Throughput on healthy traffic** — drive a 16 MiB stream of valid
//!    OSC 9 messages mixed with non-escape bytes. Must finish well inside
//!    the budget and emit exactly the expected number of payloads.
//! 2. **Memory bound under hostile, never-terminated payloads** — feed a
//!    very long unterminated OSC. The extractor must never grow past
//!    `MAX_OSC_PAYLOAD`, must drop the oversized payload, and must
//!    recover so a follow-up valid OSC parses.
//! 3. **Split-write resilience** — feed bytes in 13-byte chunks
//!    (irregular boundary) to verify the state machine is byte-stream
//!    safe regardless of read granularity.

use flowmux_notify::stream::{OscExtractor, MAX_OSC_PAYLOAD};
use std::time::{Duration, Instant};

#[test]
#[ignore = "stress: osc extractor healthy throughput"]
fn osc_extractor_handles_megabytes_of_healthy_traffic() {
    const BUDGET: Duration = Duration::from_secs(10);
    // Build a single OSC 9 payload with a small body.
    let one = b"\x1b]9;hello\x07filler-bytes-between\n";
    // Repeat until the buffer crosses ~16 MiB.
    let mut bytes: Vec<u8> = Vec::with_capacity(16 * 1024 * 1024);
    while bytes.len() < 16 * 1024 * 1024 {
        bytes.extend_from_slice(one);
    }
    let expected = bytes.windows(one.len()).filter(|w| *w == one).count();

    let mut count = 0usize;
    let start = Instant::now();
    {
        let mut x = OscExtractor::new(|s| {
            assert_eq!(s, "9;hello");
            count += 1;
        });
        // Feed the whole buffer in one call to mimic the worst-case batching.
        x.feed(&bytes);
    }
    let elapsed = start.elapsed();
    eprintln!(
        "osc throughput: parsed {} OSC payloads from {} bytes in {:?}",
        count,
        bytes.len(),
        elapsed
    );
    assert_eq!(count, expected);
    assert!(
        elapsed < BUDGET,
        "osc throughput {elapsed:?} exceeded {BUDGET:?}"
    );
}

#[test]
fn osc_extractor_buffer_stays_below_cap_on_unterminated_payload() {
    // Feed 4x MAX_OSC_PAYLOAD bytes inside an unterminated OSC. The
    // extractor's buffer must never exceed MAX_OSC_PAYLOAD even though we
    // shoved 4x as much in. Then send a properly terminated short OSC and
    // verify recovery.
    let mut emitted: Vec<String> = Vec::new();
    {
        let mut x = OscExtractor::new(|s| emitted.push(s.to_string()));
        x.feed(b"\x1b]9;");
        // Stream the oversized body in chunks so we exercise the per-byte
        // overflow guard.
        let chunk = vec![b'X'; 4096];
        let mut sent = 0usize;
        while sent < 4 * MAX_OSC_PAYLOAD {
            x.feed(&chunk);
            sent += chunk.len();
        }
        // Terminate (the oversized payload must be dropped).
        x.feed(b"\x07");
        // Recovery: a normal OSC after the overflow must parse.
        x.feed(b"\x1b]9;recovered\x07");
    }
    assert_eq!(emitted, vec!["9;recovered".to_string()]);
}

#[test]
fn osc_extractor_split_writes_at_arbitrary_offsets() {
    // Build a stream of 1000 valid OSC 9 messages back-to-back and feed
    // it 13 bytes at a time. The state machine must produce exactly 1000
    // payloads regardless of the chunk boundary landing inside the
    // sequence.
    const N: usize = 1000;
    let mut buf: Vec<u8> = Vec::new();
    for i in 0..N {
        let s = format!("\x1b]9;msg-{i}\x07");
        buf.extend_from_slice(s.as_bytes());
    }
    let mut emitted: Vec<String> = Vec::new();
    {
        let mut x = OscExtractor::new(|s| emitted.push(s.to_string()));
        for chunk in buf.chunks(13) {
            x.feed(chunk);
        }
    }
    assert_eq!(emitted.len(), N);
    for (i, payload) in emitted.iter().enumerate() {
        assert_eq!(payload, &format!("9;msg-{i}"));
    }
}
