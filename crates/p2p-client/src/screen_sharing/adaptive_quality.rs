use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::screen_quality::{ScreenShareFrameRate, ScreenShareQuality, ScreenShareResolution};

const CONTROL_WINDOW: Duration = Duration::from_secs(3);
const RECOVERY_WINDOW: Duration = Duration::from_secs(10);
const MIN_FRAMES_FOR_OVERLOAD: u64 = 45;
const MIN_PROFILE_INDEX: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AdaptiveVideoProfile {
    P1080Fps60,
    P1080Fps30,
    P720Fps30,
    P720Fps15,
}

impl AdaptiveVideoProfile {
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::P1080Fps60 => 0,
            Self::P1080Fps30 => 1,
            Self::P720Fps30 => 2,
            Self::P720Fps15 => 3,
        }
    }

    pub(crate) const fn from_index(index: usize) -> Self {
        match index {
            0 => Self::P1080Fps60,
            1 => Self::P1080Fps30,
            2 => Self::P720Fps30,
            _ => Self::P720Fps15,
        }
    }

    pub(crate) const fn frames_per_second(self) -> u32 {
        match self {
            Self::P1080Fps60 => 60,
            Self::P1080Fps30 | Self::P720Fps30 => 30,
            Self::P720Fps15 => 15,
        }
    }

    pub(crate) const fn dimensions(self) -> (u32, u32) {
        match self {
            Self::P1080Fps60 | Self::P1080Fps30 => (1920, 1080),
            Self::P720Fps30 | Self::P720Fps15 => (1280, 720),
        }
    }

    pub(crate) const fn frame_duration(self) -> Duration {
        Duration::from_nanos(1_000_000_000 / self.frames_per_second() as u64)
    }

    pub(crate) const fn cap_to_quality(self, maximum: ScreenShareQuality) -> Self {
        let maximum_index = match (maximum.resolution, maximum.frame_rate) {
            (ScreenShareResolution::P1080, ScreenShareFrameRate::Fps60) => 0,
            (ScreenShareResolution::P1080, ScreenShareFrameRate::Fps30) => 1,
            (ScreenShareResolution::P720, _) => 2,
        };
        Self::from_index(if self.index() < maximum_index {
            maximum_index
        } else {
            self.index()
        })
    }

    pub(crate) const fn suggested_bitrate_bps(self) -> u32 {
        match self {
            Self::P1080Fps60 => 8_000_000,
            Self::P1080Fps30 => 6_000_000,
            Self::P720Fps30 => 4_000_000,
            Self::P720Fps15 => 2_500_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QualityFeedbackReason {
    Overload,
    Recovered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QualityFeedback {
    pub version: u8,
    pub profile: AdaptiveVideoProfile,
    pub reason: QualityFeedbackReason,
}

impl QualityFeedback {
    pub(crate) fn parse(payload: &str) -> Option<Self> {
        let feedback: Self = serde_json::from_str(payload).ok()?;
        (feedback.version == 1).then_some(feedback)
    }

    pub(crate) fn serialize(self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QualityWindow {
    pub received: u64,
    pub published: u64,
    pub queue_drops: u64,
    pub recovery_gate_drops: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QualityOverloadEvidence {
    pub queue_drops: u64,
    pub recovery_gate_drops: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualityDecision {
    Changed {
        feedback: QualityFeedback,
        overload_evidence: Option<QualityOverloadEvidence>,
    },
    StillOverloadedAtMinimum {
        overload_evidence: QualityOverloadEvidence,
    },
}

pub(crate) struct AdaptiveQualityController {
    profile: AdaptiveVideoProfile,
    last_evaluated_at: Instant,
    recovery_elapsed: Duration,
    minimum_warning_sent: bool,
}

impl AdaptiveQualityController {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            profile: AdaptiveVideoProfile::P1080Fps60,
            last_evaluated_at: now,
            recovery_elapsed: Duration::ZERO,
            minimum_warning_sent: false,
        }
    }

    pub(crate) fn observe(
        &mut self,
        now: Instant,
        window: QualityWindow,
    ) -> Option<QualityDecision> {
        let elapsed = now.saturating_duration_since(self.last_evaluated_at);
        if elapsed < CONTROL_WINDOW {
            return None;
        }
        self.last_evaluated_at = now;
        let ratio = window.published as f64 / window.received.max(1) as f64;
        let overload_evidence = QualityOverloadEvidence {
            queue_drops: window.queue_drops,
            recovery_gate_drops: window.recovery_gate_drops,
        };
        let overloaded = window.received >= MIN_FRAMES_FOR_OVERLOAD
            && ratio < 0.80
            && (window.queue_drops >= 2 || window.recovery_gate_drops >= 2);
        if overloaded {
            self.recovery_elapsed = Duration::ZERO;
            if self.profile.index() < MIN_PROFILE_INDEX {
                self.profile = AdaptiveVideoProfile::from_index(self.profile.index() + 1);
                return Some(QualityDecision::Changed {
                    feedback: QualityFeedback {
                        version: 1,
                        profile: self.profile,
                        reason: QualityFeedbackReason::Overload,
                    },
                    overload_evidence: Some(overload_evidence),
                });
            }
            if !self.minimum_warning_sent {
                self.minimum_warning_sent = true;
                return Some(QualityDecision::StillOverloadedAtMinimum { overload_evidence });
            }
            return None;
        }

        let recovered = window.received > 0
            && ratio >= 0.90
            && window.queue_drops == 0
            && window.recovery_gate_drops == 0;
        if recovered {
            self.recovery_elapsed = self.recovery_elapsed.saturating_add(elapsed);
            if self.recovery_elapsed >= RECOVERY_WINDOW && self.profile.index() > 0 {
                self.profile = AdaptiveVideoProfile::from_index(self.profile.index() - 1);
                self.recovery_elapsed = Duration::ZERO;
                self.minimum_warning_sent = false;
                return Some(QualityDecision::Changed {
                    feedback: QualityFeedback {
                        version: 1,
                        profile: self.profile,
                        reason: QualityFeedbackReason::Recovered,
                    },
                    overload_evidence: None,
                });
            }
        } else {
            self.recovery_elapsed = Duration::ZERO;
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn profile(&self) -> AdaptiveVideoProfile {
        self.profile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overload() -> QualityWindow {
        QualityWindow {
            received: 60,
            published: 47,
            queue_drops: 2,
            recovery_gate_drops: 0,
        }
    }

    #[test]
    fn overload_requires_minimum_frames_output_ratio_and_two_drop_events() {
        let start = Instant::now();
        for window in [
            QualityWindow {
                received: 44,
                ..overload()
            },
            QualityWindow {
                published: 48,
                ..overload()
            },
            QualityWindow {
                queue_drops: 1,
                ..overload()
            },
            QualityWindow {
                queue_drops: 0,
                recovery_gate_drops: 1,
                ..overload()
            },
            QualityWindow {
                queue_drops: 1,
                recovery_gate_drops: 1,
                ..overload()
            },
        ] {
            let mut controller = AdaptiveQualityController::new(start);
            assert_eq!(controller.observe(start + CONTROL_WINDOW, window), None);
            assert_eq!(controller.profile(), AdaptiveVideoProfile::P1080Fps60);
        }
    }

    #[test]
    fn recovery_gate_drops_trigger_overload_when_decoder_queue_is_clear() {
        let start = Instant::now();
        let mut controller = AdaptiveQualityController::new(start);
        assert_eq!(
            controller.observe(
                start + CONTROL_WINDOW,
                QualityWindow {
                    queue_drops: 0,
                    recovery_gate_drops: 2,
                    ..overload()
                }
            ),
            Some(QualityDecision::Changed {
                feedback: QualityFeedback {
                    version: 1,
                    profile: AdaptiveVideoProfile::P1080Fps30,
                    reason: QualityFeedbackReason::Overload,
                },
                overload_evidence: Some(QualityOverloadEvidence {
                    queue_drops: 0,
                    recovery_gate_drops: 2,
                }),
            })
        );
    }

    #[test]
    fn overload_accepts_exact_frame_and_output_thresholds() {
        let start = Instant::now();
        let mut controller = AdaptiveQualityController::new(start);
        assert!(matches!(
            controller.observe(
                start + CONTROL_WINDOW,
                QualityWindow {
                    received: MIN_FRAMES_FOR_OVERLOAD,
                    published: 35,
                    queue_drops: 0,
                    recovery_gate_drops: 2,
                }
            ),
            Some(QualityDecision::Changed {
                feedback: QualityFeedback {
                    profile: AdaptiveVideoProfile::P1080Fps30,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn controller_waits_for_a_complete_three_second_warmup_window() {
        let start = Instant::now();
        let mut controller = AdaptiveQualityController::new(start);
        assert_eq!(
            controller.observe(
                start + CONTROL_WINDOW - Duration::from_millis(1),
                overload()
            ),
            None
        );
        assert_eq!(controller.profile(), AdaptiveVideoProfile::P1080Fps60);
        assert!(matches!(
            controller.observe(start + CONTROL_WINDOW, overload()),
            Some(QualityDecision::Changed { .. })
        ));
    }

    #[test]
    fn overload_steps_down_one_profile_per_window_and_warns_once_at_floor() {
        let start = Instant::now();
        let mut controller = AdaptiveQualityController::new(start);
        let mut last = None;
        for step in 1..=3 {
            last = controller.observe(start + CONTROL_WINDOW * step, overload());
        }
        assert!(matches!(
            last,
            Some(QualityDecision::Changed {
                feedback: QualityFeedback {
                    version: 1,
                    profile: AdaptiveVideoProfile::P720Fps15,
                    reason: QualityFeedbackReason::Overload,
                },
                overload_evidence: Some(_),
            })
        ));
        assert_eq!(
            controller.observe(start + CONTROL_WINDOW * 4, overload()),
            Some(QualityDecision::StillOverloadedAtMinimum {
                overload_evidence: QualityOverloadEvidence {
                    queue_drops: 2,
                    recovery_gate_drops: 0,
                }
            })
        );
        assert_eq!(
            controller.observe(start + CONTROL_WINDOW * 5, overload()),
            None
        );
    }

    #[test]
    fn recovery_requires_ten_healthy_seconds_then_steps_up_once() {
        let start = Instant::now();
        let mut controller = AdaptiveQualityController::new(start);
        for step in 1..=3 {
            controller.observe(start + CONTROL_WINDOW * step, overload());
        }
        let healthy = QualityWindow {
            received: 60,
            published: 54,
            queue_drops: 0,
            recovery_gate_drops: 0,
        };
        assert_eq!(
            controller.observe(start + Duration::from_secs(12), healthy),
            None
        );
        assert_eq!(
            controller.observe(start + Duration::from_secs(15), healthy),
            None
        );
        assert_eq!(
            controller.observe(start + Duration::from_secs(18), healthy),
            None
        );
        let decision = controller.observe(start + Duration::from_secs(21), healthy);
        assert_eq!(
            decision,
            Some(QualityDecision::Changed {
                feedback: QualityFeedback {
                    version: 1,
                    profile: AdaptiveVideoProfile::P720Fps30,
                    reason: QualityFeedbackReason::Recovered,
                },
                overload_evidence: None,
            })
        );
    }

    #[test]
    fn recovery_waits_until_recovery_gate_drops_stop() {
        let start = Instant::now();
        let mut controller = AdaptiveQualityController::new(start);
        controller.observe(start + CONTROL_WINDOW, overload());
        let healthy = QualityWindow {
            received: 60,
            published: 54,
            queue_drops: 0,
            recovery_gate_drops: 0,
        };
        for step in 2..=4 {
            controller.observe(start + CONTROL_WINDOW * step, healthy);
        }
        let with_recovery_drop = QualityWindow {
            recovery_gate_drops: 1,
            ..healthy
        };
        assert_eq!(
            controller.observe(start + CONTROL_WINDOW * 5, with_recovery_drop),
            None
        );
        for step in 6..=8 {
            assert_eq!(
                controller.observe(start + CONTROL_WINDOW * step, healthy),
                None
            );
        }
        assert!(matches!(
            controller.observe(start + CONTROL_WINDOW * 9, healthy),
            Some(QualityDecision::Changed {
                feedback: QualityFeedback {
                    reason: QualityFeedbackReason::Recovered,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn sender_profile_is_limited_by_selected_quality() {
        let p72030 = ScreenShareQuality::default();
        assert_eq!(
            AdaptiveVideoProfile::P1080Fps60.cap_to_quality(p72030),
            AdaptiveVideoProfile::P720Fps30
        );
        assert_eq!(
            AdaptiveVideoProfile::P720Fps15.cap_to_quality(p72030),
            AdaptiveVideoProfile::P720Fps15
        );
        let p1080_30 = ScreenShareQuality {
            resolution: ScreenShareResolution::P1080,
            frame_rate: ScreenShareFrameRate::Fps30,
        };
        assert_eq!(
            AdaptiveVideoProfile::P1080Fps60.cap_to_quality(p1080_30),
            AdaptiveVideoProfile::P1080Fps30
        );
    }

    #[test]
    fn feedback_is_versioned_and_invalid_payloads_are_ignored() {
        let feedback = QualityFeedback {
            version: 1,
            profile: AdaptiveVideoProfile::P720Fps15,
            reason: QualityFeedbackReason::Overload,
        };
        let payload = feedback.serialize().unwrap();
        assert_eq!(QualityFeedback::parse(&payload), Some(feedback));
        assert_eq!(QualityFeedback::parse("{broken"), None);
        assert_eq!(
            QualityFeedback::parse(r#"{"version":2,"profile":"p720_fps15","reason":"overload"}"#),
            None
        );
    }
}
