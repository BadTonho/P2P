use std::time::{Duration, Instant};

pub(super) trait InputEventPump {
    fn poll_input_credit(&mut self, wait: Duration) -> Result<bool, String>;
    fn drain_pending_output(&mut self) -> Result<(), String>;
}

// A hardware MFT can withhold NeedInput until HaveOutput is consumed. Waiting
// only for input leaves that output queued and incorrectly trips the fallback.
pub(super) fn wait_for_input(
    pump: &mut impl InputEventPump,
    timeout: Duration,
) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if pump.poll_input_credit(remaining.min(Duration::from_millis(1)))? {
            return Ok(true);
        }
        pump.drain_pending_output()?;
        if Instant::now() >= deadline {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BackpressuredCodec {
        input_ready: bool,
        output_pending: bool,
        saved_outputs: usize,
        failed: bool,
    }

    impl InputEventPump for BackpressuredCodec {
        fn poll_input_credit(&mut self, _: Duration) -> Result<bool, String> {
            if self.failed {
                return Err("codec error".to_owned());
            }
            Ok(self.input_ready)
        }

        fn drain_pending_output(&mut self) -> Result<(), String> {
            if self.output_pending {
                self.output_pending = false;
                self.saved_outputs += 1;
                self.input_ready = true;
            }
            Ok(())
        }
    }

    #[test]
    fn pending_output_is_saved_and_unblocks_the_next_input() {
        let mut codec = BackpressuredCodec {
            input_ready: false,
            output_pending: true,
            saved_outputs: 0,
            failed: false,
        };
        assert!(wait_for_input(&mut codec, Duration::from_secs(1)).unwrap());
        assert_eq!(codec.saved_outputs, 1);
    }

    #[test]
    fn ready_input_does_not_consume_unrelated_output() {
        let mut codec = BackpressuredCodec {
            input_ready: true,
            output_pending: true,
            saved_outputs: 0,
            failed: false,
        };
        assert!(wait_for_input(&mut codec, Duration::ZERO).unwrap());
        assert_eq!(codec.saved_outputs, 0);
    }

    #[test]
    fn timeout_and_codec_errors_are_preserved() {
        let mut codec = BackpressuredCodec {
            input_ready: false,
            output_pending: false,
            saved_outputs: 0,
            failed: false,
        };
        assert!(!wait_for_input(&mut codec, Duration::ZERO).unwrap());
        codec.failed = true;
        assert_eq!(
            wait_for_input(&mut codec, Duration::ZERO).unwrap_err(),
            "codec error"
        );
    }
}
