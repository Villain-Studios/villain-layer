//! What a pane has printed, kept by position so a reader can be sent
//! exactly what it missed.

/// Roughly one screenful of history per pane, replayed when a terminal is
/// first drawn or has fallen too far behind to catch up.
const SCROLLBACK_LIMIT: usize = 256 * 1024;

/// How far past the limit scrollback may run before it is trimmed.
///
/// Trimming on every read once full meant moving 256KB to append a few bytes,
/// hundreds of times a second for an agent that is redrawing. Letting it
/// overshoot makes the move rare.
const SCROLLBACK_SLACK: usize = 64 * 1024;

/// What a pane has printed, and how much of it the webview has been sent.
///
/// Positions are counted from the pane's first byte, so the webview can say
/// exactly how much it already has. That is what lets a terminal coming back
/// on screen be sent only what it missed, instead of being wiped and
/// repainted from the whole scrollback — which is the flash, and the pause,
/// every time a task or a tab was switched.
pub(super) struct Output {
    /// The most recent output, from `total - scrollback.len()` to `total`.
    pub(super) scrollback: Vec<u8>,
    /// Every byte the process has printed.
    pub(super) total: u64,
    /// Where the webview's feed has got to. Anything after this is waiting
    /// for the next flush.
    pub(super) sent: u64,
}

impl Output {
    fn start(&self) -> u64 {
        self.total - self.scrollback.len() as u64
    }

    pub(super) fn push(&mut self, chunk: &[u8]) {
        self.scrollback.extend_from_slice(chunk);
        self.total += chunk.len() as u64;
        if self.scrollback.len() > SCROLLBACK_LIMIT + SCROLLBACK_SLACK {
            let mut cut = self.scrollback.len() - SCROLLBACK_LIMIT;
            // Start the kept history at a line, so a replay does not open on
            // the back half of an escape sequence or of a UTF-8 character.
            let look = &self.scrollback[cut..(cut + 4096).min(self.scrollback.len())];
            if let Some(nl) = look.iter().position(|&b| b == b'\n') {
                cut += nl + 1;
            }
            self.scrollback.drain(..cut);
        }
    }

    /// Output since `from`, or all of it when `from` is no longer held.
    pub(super) fn since(&self, from: Option<u64>) -> (&[u8], bool) {
        let start = self.start();
        match from {
            Some(f) if f >= start && f <= self.total => {
                (&self.scrollback[(f - start) as usize..], false)
            }
            // A terminal that has never been drawn has nothing to clear.
            // One that is too far behind has to start again.
            other => (&self.scrollback, other.is_some()),
        }
    }

    /// The output the feed has not carried yet, marking it carried.
    pub(super) fn take_unsent(&mut self) -> Option<(Vec<u8>, u64)> {
        if self.sent >= self.total {
            return None;
        }
        // More than the scrollback holds went by between flushes. Send what
        // is left; the webview sees the gap and asks for a replay.
        let from = self.sent.max(self.start());
        let bytes = self.scrollback[(from - self.start()) as usize..].to_vec();
        self.sent = self.total;
        Some((bytes, self.total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> Output {
        Output { scrollback: Vec::new(), total: 0, sent: 0 }
    }

    #[test]
    fn a_terminal_that_kept_up_is_sent_only_what_it_missed() {
        let mut out = output();
        out.push(b"hello ");
        out.push(b"world");
        assert_eq!(out.since(Some(6)), (&b"world"[..], false));
        assert_eq!(out.since(Some(11)), (&b""[..], false));
        // Never drawn: everything, and nothing to clear.
        assert_eq!(out.since(None), (&b"hello world"[..], false));
    }

    #[test]
    fn trimming_keeps_positions_and_starts_on_a_line() {
        let mut out = output();
        let line = [b'x'; 1023].iter().chain(b"\n").copied().collect::<Vec<u8>>();
        while out.total < (SCROLLBACK_LIMIT + SCROLLBACK_SLACK + 4096) as u64 {
            out.push(&line);
        }
        assert!(out.scrollback.len() <= SCROLLBACK_LIMIT + SCROLLBACK_SLACK);
        assert_eq!(out.start() + out.scrollback.len() as u64, out.total);
        // The kept history opens on a fresh line, not halfway through one.
        assert_eq!(out.scrollback[0], b'x');
        assert_eq!(out.start() % line.len() as u64, 0);

        // A point that has been trimmed away means starting again.
        let (all, reset) = out.since(Some(0));
        assert!(reset);
        assert_eq!(all.len(), out.scrollback.len());
        let (tail, reset) = out.since(Some(out.total - 3));
        assert!(!reset);
        assert_eq!(tail, b"xx\n");
    }

    #[test]
    fn the_feed_carries_each_byte_once() {
        let mut out = output();
        out.push(b"abc");
        assert_eq!(out.take_unsent(), Some((b"abc".to_vec(), 3)));
        assert_eq!(out.take_unsent(), None);
        out.push(b"de");
        assert_eq!(out.take_unsent(), Some((b"de".to_vec(), 5)));
    }
}
