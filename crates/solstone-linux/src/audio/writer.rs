// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::path::Path;

use flac_bound::FlacEncoder;

use crate::{
    chunking::to_i16,
    encoding::{AudioFileSource, AudioOutputPlan},
    observer::AudioWriter,
};

#[derive(Default)]
pub(crate) struct FlacAudioWriter;

impl AudioWriter for FlacAudioWriter {
    fn write(
        &mut self,
        frames: &[f32],
        plan: &AudioOutputPlan,
        directory: &Path,
    ) -> Result<(), String> {
        for file in &plan.files {
            let samples: Vec<i32> = match file.source {
                AudioFileSource::StereoInterleaved => frames
                    .iter()
                    .map(|&sample| i32::from(to_i16(sample)))
                    .collect(),
                AudioFileSource::MicrophoneMono => frames
                    .chunks_exact(2)
                    .map(|frame| i32::from(to_i16(frame[0])))
                    .collect(),
                AudioFileSource::SystemMono => frames
                    .chunks_exact(2)
                    .map(|frame| i32::from(to_i16(frame[1])))
                    .collect(),
            };
            let mut encoder = FlacEncoder::new()
                .ok_or("failed to allocate FLAC encoder")?
                .sample_rate(plan.sample_rate)
                .bits_per_sample(plan.bits_per_sample)
                .channels(file.channels)
                .init_file(&directory.join(file.filename))
                .map_err(|error| format!("failed to initialize FLAC encoder: {error:?}"))?;
            let frame_count = u32::try_from(samples.len() / file.channels as usize)
                .map_err(|error| error.to_string())?;
            encoder
                .process_interleaved(&samples, frame_count)
                .map_err(|()| "FLAC encoding failed".to_owned())?;
            encoder
                .finish()
                .map_err(|_| "FLAC finalization failed".to_owned())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::audio_output_plan;

    #[test]
    fn writes_stereo_and_split_mono_flac() {
        // tests/test_audio_recorder.py::test_create_flac_and_mono_flac_bytes_nonempty
        let temp = tempfile::tempdir().unwrap();
        let frames = [0.1, 0.2, 0.3, 0.4];
        let mut writer = FlacAudioWriter;
        writer
            .write(&frames, &audio_output_plan(false), temp.path())
            .unwrap();
        assert!(
            std::fs::read(temp.path().join("audio.flac"))
                .unwrap()
                .starts_with(b"fLaC")
        );
        writer
            .write(&frames, &audio_output_plan(true), temp.path())
            .unwrap();
        assert!(
            std::fs::read(temp.path().join("mic_audio.flac"))
                .unwrap()
                .starts_with(b"fLaC")
        );
        assert!(
            std::fs::read(temp.path().join("sys_audio.flac"))
                .unwrap()
                .starts_with(b"fLaC")
        );
    }
}
