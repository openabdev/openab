//! Bounded buffers on the PTY-to-client output path.
//!
//! The ADR's "Output-path bounds (MUST)" clause requires *every* buffer on that
//! path to be bounded, not just retained scrollback. Three of them live here:
//!
//! | Buffer | Type | Bound | Overflow behaviour |
//! |---|---|---|---|
//! | retained scrollback | [`RingBuffer`] | `scrollback_kib` | drop oldest, cursor advances, drop count exposed |
//! | replay-to-live handoff queue | [`ByteQueue`] | `handoff_queue_bytes` | drop oldest, report a `gap` |
//! | per-connection outbound backlog | [`ByteQueue`] | `backlog_bytes` | drop oldest, report a `gap`; past the hard watermark the caller closes with [`close_code::SLOW_CLIENT`] |
//!
//! The scrollback cursor is adopted from OpenDray's `ringbuf.go` (ADR §6): a
//! monotonic *total bytes written* counter, so a client reconnecting with
//! `since=<offset>` receives exactly the bytes it missed. Overflow is never
//! silently sliced — a sliced ANSI stream renders as garbage, so the drop count
//! is surfaced and the caller emits an explicit `gap` control frame instead.
//!
//! [`close_code::SLOW_CLIENT`]: crate::close_code::SLOW_CLIENT

use std::collections::VecDeque;

/// Retained scrollback with a monotonic total-bytes-written cursor.
///
/// `capacity == 0` disables retention entirely, which per the ADR also disables
/// `since` replay: every reconnect then starts with a `gap` + reset.
#[derive(Debug)]
pub struct RingBuffer {
    buf: VecDeque<u8>,
    capacity: usize,
    /// Total bytes ever written. Monotonic; never reset by eviction, only by an
    /// explicit [`RingBuffer::reset`] (restart-in-place gets a fresh cursor).
    written: u64,
    dropped: u64,
}

/// Result of a `since=<offset>` replay query.
///
/// The distinction is load-bearing: [`Replay::Contiguous`] bytes can be written
/// straight into the client's emulator, while [`Replay::Gap`] means the client
/// must clear and redraw because the stream it holds and the bytes it is about
/// to receive are not adjacent.
#[derive(Debug, PartialEq, Eq)]
pub enum Replay {
    /// `bytes` continue exactly at the requested offset.
    Contiguous { bytes: Vec<u8>, to_offset: u64 },
    /// Bytes between the client's cursor and `bytes` were evicted. The caller
    /// MUST emit a `gap` control frame carrying `bytes_dropped` before writing
    /// `bytes`.
    Gap {
        bytes_dropped: u64,
        bytes: Vec<u8>,
        to_offset: u64,
    },
}

impl RingBuffer {
    /// Construct from the operator-facing `scrollback_kib` knob.
    pub fn with_scrollback_kib(kib: usize) -> Self {
        Self::new(kib.saturating_mul(1024))
    }

    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity_bytes.min(64 * 1024)),
            capacity: capacity_bytes,
            written: 0,
            dropped: 0,
        }
    }

    /// `false` when `scrollback_kib = 0`: nothing is retained and `since`
    /// replay is unavailable.
    pub fn retention_enabled(&self) -> bool {
        self.capacity > 0
    }

    /// Append PTY output. Returns the number of bytes evicted (or, with
    /// retention disabled, discarded) by *this* write, so the caller can emit a
    /// `gap` frame instead of silently slicing the stream.
    pub fn write(&mut self, bytes: &[u8]) -> u64 {
        let n = bytes.len();
        self.written = self.written.saturating_add(n as u64);

        if self.capacity == 0 {
            self.dropped = self.dropped.saturating_add(n as u64);
            return n as u64;
        }

        // Bytes that cannot fit once `bytes` is appended: evicted from the head
        // of the retained window first, then from the head of `bytes` itself
        // when a single write is larger than the whole window.
        let overflow = (self.buf.len() + n).saturating_sub(self.capacity);
        if overflow == 0 {
            self.buf.extend(bytes);
            return 0;
        }

        let from_retained = overflow.min(self.buf.len());
        self.buf.drain(..from_retained);
        let from_incoming = overflow - from_retained;
        self.buf.extend(&bytes[from_incoming..]);
        self.dropped = self.dropped.saturating_add(overflow as u64);
        overflow as u64
    }

    /// Monotonic cursor: total bytes ever written to this session's stream.
    pub fn total_written(&self) -> u64 {
        self.written
    }

    /// Total bytes evicted or discarded over the session's lifetime.
    pub fn total_dropped(&self) -> u64 {
        self.dropped
    }

    /// Offset of the oldest retained byte.
    pub fn earliest_offset(&self) -> u64 {
        self.written - self.buf.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Everything currently retained, oldest first.
    pub fn snapshot(&self) -> Vec<u8> {
        self.slice_from(0)
    }

    /// Bytes the client with cursor `since` has not seen.
    ///
    /// `since > total_written` cannot happen for a well-behaved client on this
    /// generation; it is treated as a gap + reset rather than an error, because
    /// the benign cause is a cursor carried over from a previous generation
    /// (restart-in-place resets the stream).
    pub fn read_since(&self, since: u64) -> Replay {
        if !self.retention_enabled() {
            // Retention disabled: nothing to replay, and the client must assume
            // everything since its cursor is lost.
            return Replay::Gap {
                bytes_dropped: self.written.saturating_sub(since),
                bytes: Vec::new(),
                to_offset: self.written,
            };
        }
        if since > self.written {
            return Replay::Gap {
                bytes_dropped: 0,
                bytes: self.snapshot(),
                to_offset: self.written,
            };
        }
        let earliest = self.earliest_offset();
        if since >= earliest {
            return Replay::Contiguous {
                bytes: self.slice_from((since - earliest) as usize),
                to_offset: self.written,
            };
        }
        Replay::Gap {
            bytes_dropped: earliest - since,
            bytes: self.snapshot(),
            to_offset: self.written,
        }
    }

    /// Drop retained bytes but keep the cursor monotonic. Used on
    /// session-teardown: buffers are cleared and scrollback never touches disk.
    pub fn clear(&mut self) {
        self.dropped = self.dropped.saturating_add(self.buf.len() as u64);
        self.buf.clear();
    }

    fn slice_from(&self, start: usize) -> Vec<u8> {
        let (a, b) = self.buf.as_slices();
        if start >= self.buf.len() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(self.buf.len() - start);
        if start < a.len() {
            out.extend_from_slice(&a[start..]);
            out.extend_from_slice(b);
        } else {
            out.extend_from_slice(&b[start - a.len()..]);
        }
        out
    }
}

/// Bounds for a [`ByteQueue`].
#[derive(Debug, Clone, Copy)]
pub struct QueueBounds {
    /// Retained bytes cap. Exceeding it evicts the oldest bytes.
    pub capacity_bytes: usize,
    /// Hard watermark on *cumulative* dropped bytes for this queue. Once past
    /// it the client is hopeless rather than momentarily slow, and the caller
    /// disconnects it with [`crate::close_code::SLOW_CLIENT`]. `None` disables
    /// the disconnect (drop-oldest forever) — not recommended for the
    /// per-connection backlog.
    pub max_dropped_bytes: Option<u64>,
}

impl QueueBounds {
    pub fn new(capacity_bytes: usize, max_dropped_bytes: Option<u64>) -> Self {
        Self {
            capacity_bytes,
            max_dropped_bytes,
        }
    }
}

/// Outcome of a [`ByteQueue::push`].
#[derive(Debug, PartialEq, Eq)]
pub enum Push {
    /// Everything fit.
    Accepted,
    /// Oldest bytes were evicted to make room; the caller emits a `gap` frame.
    DroppedOldest { bytes_dropped: u64 },
    /// Cumulative drops passed the hard watermark: disconnect the client with
    /// [`crate::close_code::SLOW_CLIENT`]. Bytes were still enqueued
    /// drop-oldest, so a caller that ignores this stays bounded.
    HardWatermark { bytes_dropped: u64 },
}

/// A bounded FIFO byte queue used for both the replay-to-live handoff and the
/// per-connection outbound backlog.
///
/// `unreported_dropped` accumulates so a writer that falls behind emits *one*
/// `gap` frame per catch-up burst instead of one per evicting push.
#[derive(Debug)]
pub struct ByteQueue {
    buf: VecDeque<u8>,
    bounds: QueueBounds,
    dropped: u64,
    unreported_dropped: u64,
}

impl ByteQueue {
    pub fn new(bounds: QueueBounds) -> Self {
        Self {
            buf: VecDeque::new(),
            bounds,
            dropped: 0,
            unreported_dropped: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Push {
        let n = bytes.len();
        if self.bounds.capacity_bytes == 0 {
            self.account_drop(n as u64);
            return self.overflow_outcome(n as u64);
        }
        let overflow = (self.buf.len() + n).saturating_sub(self.bounds.capacity_bytes);
        if overflow == 0 {
            self.buf.extend(bytes);
            return Push::Accepted;
        }
        let from_retained = overflow.min(self.buf.len());
        self.buf.drain(..from_retained);
        let from_incoming = overflow - from_retained;
        self.buf.extend(&bytes[from_incoming..]);
        self.account_drop(overflow as u64);
        self.overflow_outcome(overflow as u64)
    }

    fn account_drop(&mut self, n: u64) {
        self.dropped = self.dropped.saturating_add(n);
        self.unreported_dropped = self.unreported_dropped.saturating_add(n);
    }

    fn overflow_outcome(&self, bytes_dropped: u64) -> Push {
        match self.bounds.max_dropped_bytes {
            Some(limit) if self.dropped > limit => Push::HardWatermark { bytes_dropped },
            _ => Push::DroppedOldest { bytes_dropped },
        }
    }

    /// Take up to `max` bytes for the socket.
    pub fn take(&mut self, max: usize) -> Vec<u8> {
        let n = max.min(self.buf.len());
        self.buf.drain(..n).collect()
    }

    /// Drop count not yet surfaced to the client, cleared by this call so
    /// exactly one `gap` frame is emitted per burst.
    pub fn take_unreported_gap(&mut self) -> Option<u64> {
        if self.unreported_dropped == 0 {
            None
        } else {
            Some(std::mem::take(&mut self.unreported_dropped))
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_is_monotonic_across_eviction() {
        let mut r = RingBuffer::new(8);
        assert_eq!(r.write(b"abcdefgh"), 0);
        assert_eq!(r.total_written(), 8);
        assert_eq!(r.earliest_offset(), 0);

        // Overflow by 4: cursor keeps counting, window slides.
        assert_eq!(r.write(b"ijkl"), 4);
        assert_eq!(r.total_written(), 12);
        assert_eq!(r.earliest_offset(), 4);
        assert_eq!(r.snapshot(), b"efghijkl");
    }

    #[test]
    fn replay_since_returns_only_missed_bytes() {
        let mut r = RingBuffer::new(64);
        r.write(b"hello ");
        r.write(b"world");
        match r.read_since(6) {
            Replay::Contiguous { bytes, to_offset } => {
                assert_eq!(bytes, b"world");
                assert_eq!(to_offset, 11);
            }
            other => panic!("expected contiguous replay, got {other:?}"),
        }
    }

    #[test]
    fn replay_at_head_is_empty_and_contiguous() {
        let mut r = RingBuffer::new(64);
        r.write(b"abc");
        assert_eq!(
            r.read_since(3),
            Replay::Contiguous {
                bytes: Vec::new(),
                to_offset: 3
            }
        );
    }

    #[test]
    fn replay_past_eviction_reports_exact_drop_count() {
        let mut r = RingBuffer::new(4);
        r.write(b"abcdefgh"); // window now "efgh", earliest = 4
        match r.read_since(1) {
            Replay::Gap {
                bytes_dropped,
                bytes,
                to_offset,
            } => {
                assert_eq!(bytes_dropped, 3, "offsets 1..4 were evicted");
                assert_eq!(bytes, b"efgh");
                assert_eq!(to_offset, 8);
            }
            other => panic!("expected gap, got {other:?}"),
        }
    }

    #[test]
    fn single_write_larger_than_window_keeps_tail() {
        let mut r = RingBuffer::new(4);
        assert_eq!(r.write(b"abcdefgh"), 4);
        assert_eq!(r.snapshot(), b"efgh");
        assert_eq!(r.total_written(), 8);
        assert_eq!(r.total_dropped(), 4);
    }

    #[test]
    fn zero_scrollback_disables_retention_and_since_replay() {
        let mut r = RingBuffer::with_scrollback_kib(0);
        assert!(!r.retention_enabled());
        assert_eq!(r.write(b"abcdef"), 6);
        assert!(r.is_empty());
        // Every reconnect is a gap + reset, whatever the cursor says.
        match r.read_since(0) {
            Replay::Gap {
                bytes_dropped,
                bytes,
                to_offset,
            } => {
                assert_eq!(bytes_dropped, 6);
                assert!(bytes.is_empty());
                assert_eq!(to_offset, 6);
            }
            other => panic!("expected gap, got {other:?}"),
        }
        assert!(matches!(r.read_since(6), Replay::Gap { .. }));
    }

    #[test]
    fn cursor_from_another_generation_is_a_gap_not_an_error() {
        let mut r = RingBuffer::new(64);
        r.write(b"abc");
        match r.read_since(9_000) {
            Replay::Gap {
                bytes_dropped,
                bytes,
                ..
            } => {
                assert_eq!(bytes_dropped, 0);
                assert_eq!(bytes, b"abc", "client clears and redraws what we have");
            }
            other => panic!("expected gap, got {other:?}"),
        }
    }

    #[test]
    fn clear_keeps_the_cursor() {
        let mut r = RingBuffer::new(64);
        r.write(b"abc");
        r.clear();
        assert_eq!(
            r.total_written(),
            3,
            "teardown clears bytes, not the cursor"
        );
        assert!(r.is_empty());
    }

    #[test]
    fn wrapped_window_slices_in_order() {
        let mut r = RingBuffer::new(6);
        r.write(b"abcdef");
        r.write(b"ghi"); // wraps the VecDeque internals
        assert_eq!(r.snapshot(), b"defghi");
        match r.read_since(7) {
            Replay::Contiguous { bytes, .. } => assert_eq!(bytes, b"hi"),
            other => panic!("expected contiguous, got {other:?}"),
        }
    }

    #[test]
    fn queue_accepts_within_capacity() {
        let mut q = ByteQueue::new(QueueBounds::new(16, Some(64)));
        assert_eq!(q.push(b"1234"), Push::Accepted);
        assert_eq!(q.len(), 4);
        assert_eq!(q.take(2), b"12");
        assert_eq!(q.take(99), b"34");
        assert!(q.is_empty());
        assert_eq!(q.take_unreported_gap(), None);
    }

    #[test]
    fn queue_drops_oldest_and_reports_one_gap_per_burst() {
        let mut q = ByteQueue::new(QueueBounds::new(4, Some(1024)));
        q.push(b"abcd");
        assert_eq!(q.push(b"ef"), Push::DroppedOldest { bytes_dropped: 2 });
        assert_eq!(q.push(b"gh"), Push::DroppedOldest { bytes_dropped: 2 });
        assert_eq!(q.len(), 4, "queue stays bounded");
        assert_eq!(
            q.take_unreported_gap(),
            Some(4),
            "coalesced into a single gap frame"
        );
        assert_eq!(q.take_unreported_gap(), None);
    }

    #[test]
    fn queue_signals_hard_watermark_for_slow_client_disconnect() {
        let mut q = ByteQueue::new(QueueBounds::new(4, Some(4)));
        q.push(b"abcd");
        assert_eq!(q.push(b"efgh"), Push::DroppedOldest { bytes_dropped: 4 });
        // Cumulative drops now exceed the watermark.
        assert!(matches!(q.push(b"ij"), Push::HardWatermark { .. }));
        assert!(q.len() <= 4, "still bounded even past the watermark");
    }

    #[test]
    fn zero_capacity_queue_drops_everything() {
        let mut q = ByteQueue::new(QueueBounds::new(0, None));
        assert_eq!(q.push(b"abc"), Push::DroppedOldest { bytes_dropped: 3 });
        assert!(q.is_empty());
        assert_eq!(q.take_unreported_gap(), Some(3));
    }
}
