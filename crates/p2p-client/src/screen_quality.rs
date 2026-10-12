use std::time::Duration;

use serde::{Deserialize, Serialize};

const NANOS_PER_SECOND: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScreenShareResolution {
    #[default]
    P720,
    P1080,
}

impl ScreenShareResolution {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::P720 => "720p",
            Self::P1080 => "1080p",
        }
    }

    pub(crate) const fn max_dimensions(self) -> (u32, u32) {
        match self {
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScreenShareFrameRate {
    #[default]
    Fps30,
    Fps60,
}

impl ScreenShareFrameRate {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Fps30 => "30 FPS",
            Self::Fps60 => "60 FPS",
        }
    }

    pub(crate) const fn frames_per_second(self) -> u32 {
        match self {
            Self::Fps30 => 30,
            Self::Fps60 => 60,
        }
    }

    pub(crate) const fn frame_duration(self) -> Duration {
        Duration::from_nanos(NANOS_PER_SECOND / self.frames_per_second() as u64)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ScreenShareQuality {
    pub(crate) resolution: ScreenShareResolution,
    pub(crate) frame_rate: ScreenShareFrameRate,
}

impl ScreenShareQuality {
    pub(crate) const fn max_dimensions(self) -> (u32, u32) {
        self.resolution.max_dimensions()
    }

    pub(crate) const fn frames_per_second(self) -> u32 {
        self.frame_rate.frames_per_second()
    }

    pub(crate) const fn frame_duration(self) -> Duration {
        self.frame_rate.frame_duration()
    }

    pub(crate) const fn individual_bitrate_bps(self) -> u32 {
        match (self.resolution, self.frame_rate) {
            (ScreenShareResolution::P720, ScreenShareFrameRate::Fps30) => 4_000_000,
            (ScreenShareResolution::P1080, ScreenShareFrameRate::Fps60) => 8_000_000,
            _ => 6_000_000,
        }
    }

    pub(crate) const fn group_aggregate_bitrate_bps(self) -> u32 {
        match (self.resolution, self.frame_rate) {
            (ScreenShareResolution::P720, ScreenShareFrameRate::Fps30) => 8_000_000,
            (ScreenShareResolution::P1080, ScreenShareFrameRate::Fps60) => 16_000_000,
            _ => 12_000_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ScreenShareFrameRate as Fps, ScreenShareQuality, ScreenShareResolution as Resolution,
    };

    #[test]
    fn all_profiles_have_the_expected_limits_and_budgets() {
        let profiles = [
            (
                Resolution::P720,
                Fps::Fps30,
                (1280, 720),
                4_000_000,
                8_000_000,
            ),
            (
                Resolution::P720,
                Fps::Fps60,
                (1280, 720),
                6_000_000,
                12_000_000,
            ),
            (
                Resolution::P1080,
                Fps::Fps30,
                (1920, 1080),
                6_000_000,
                12_000_000,
            ),
            (
                Resolution::P1080,
                Fps::Fps60,
                (1920, 1080),
                8_000_000,
                16_000_000,
            ),
        ];

        for (resolution, frame_rate, dimensions, individual, aggregate) in profiles {
            let quality = ScreenShareQuality {
                resolution,
                frame_rate,
            };
            assert_eq!(quality.max_dimensions(), dimensions);
            assert_eq!(quality.individual_bitrate_bps(), individual);
            assert_eq!(quality.group_aggregate_bitrate_bps(), aggregate);
            assert_eq!(
                quality.frame_duration().as_nanos(),
                1_000_000_000 / u128::from(frame_rate.frames_per_second())
            );
        }
    }

    #[test]
    fn legacy_default_profile_is_720p30() {
        let quality = ScreenShareQuality::default();
        assert_eq!(quality.resolution, Resolution::P720);
        assert_eq!(quality.frame_rate, Fps::Fps30);
    }
}
