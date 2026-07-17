// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::chunking::SAMPLE_RATE;

pub const BITS_PER_SAMPLE: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFileSource {
    StereoInterleaved,
    MicrophoneMono,
    SystemMono,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFilePlan {
    pub filename: &'static str,
    pub channels: u32,
    pub source: AudioFileSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioOutputPlan {
    pub sample_rate: u32,
    pub bits_per_sample: u32,
    pub files: Vec<AudioFilePlan>,
}

pub fn audio_output_plan(muted: bool) -> AudioOutputPlan {
    let files = if muted {
        vec![
            AudioFilePlan {
                filename: "mic_audio.flac",
                channels: 1,
                source: AudioFileSource::MicrophoneMono,
            },
            AudioFilePlan {
                filename: "sys_audio.flac",
                channels: 1,
                source: AudioFileSource::SystemMono,
            },
        ]
    } else {
        vec![AudioFilePlan {
            filename: "audio.flac",
            channels: 2,
            source: AudioFileSource::StereoInterleaved,
        }]
    };
    AudioOutputPlan {
        sample_rate: SAMPLE_RATE,
        bits_per_sample: BITS_PER_SAMPLE,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmuted_plan_is_one_stereo_file() {
        let plan = audio_output_plan(false);
        assert_eq!(plan.sample_rate, 16_000);
        assert_eq!(plan.bits_per_sample, 16);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].filename, "audio.flac");
        assert_eq!(plan.files[0].channels, 2);
        assert_eq!(plan.files[0].source, AudioFileSource::StereoInterleaved);
    }

    #[test]
    fn muted_plan_is_two_mono_files() {
        let plan = audio_output_plan(true);
        assert_eq!(plan.sample_rate, 16_000);
        assert_eq!(plan.bits_per_sample, 16);
        assert_eq!(
            plan.files
                .iter()
                .map(|file| (file.filename, file.channels, file.source))
                .collect::<Vec<_>>(),
            vec![
                ("mic_audio.flac", 1, AudioFileSource::MicrophoneMono),
                ("sys_audio.flac", 1, AudioFileSource::SystemMono),
            ]
        );
    }
}
