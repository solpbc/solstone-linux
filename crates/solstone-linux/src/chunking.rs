// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::VecDeque;

pub const SAMPLE_RATE: u32 = 16_000;
pub const BLOCK_SIZE: usize = 1_024;
pub const RMS_THRESHOLD: f32 = 0.01;
pub const MIN_HITS_FOR_SAVE: usize = 3;
const MAX_RETAINED_SAMPLES: usize = SAMPLE_RATE as usize * 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioLeg {
    Microphone,
    System,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LegBlock {
    pub leg: AudioLeg,
    pub samples: Vec<f32>,
}

impl LegBlock {
    pub fn new(leg: AudioLeg, samples: Vec<f32>) -> Self {
        let samples = samples
            .into_iter()
            .map(|sample| {
                if sample.is_nan() {
                    0.0
                } else if sample == f32::INFINITY {
                    1e10
                } else if sample == f32::NEG_INFINITY {
                    -1e10
                } else {
                    sample
                }
            })
            .collect();
        Self { leg, samples }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegReport {
    pub received_samples: usize,
    pub paired_samples: usize,
    pub retained_backlog_samples: usize,
    pub dropped_overflow_samples: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PairingReport {
    pub microphone: LegReport,
    pub system: LegReport,
}

impl PairingReport {
    pub fn is_imbalanced(&self) -> bool {
        (self.microphone.received_samples == 0 && self.system.received_samples != 0)
            || (self.system.received_samples == 0 && self.microphone.received_samples != 0)
            || self.microphone.dropped_overflow_samples != 0
            || self.system.dropped_overflow_samples != 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrainedChunk {
    interleaved: Vec<f32>,
    pub report: PairingReport,
}

impl DrainedChunk {
    pub fn interleaved(&self) -> &[f32] {
        &self.interleaved
    }

    pub fn frames(&self) -> usize {
        self.interleaved.len() / 2
    }

    pub fn rms(&self) -> f32 {
        if self.interleaved.is_empty() {
            return 0.0;
        }
        let frames = self.frames() as f64;
        let mut left = 0.0_f64;
        let mut right = 0.0_f64;
        for frame in self.interleaved.chunks_exact(2) {
            left += f64::from(frame[0]).powi(2);
            right += f64::from(frame[1]).powi(2);
        }
        ((left / frames).sqrt().max((right / frames).sqrt())) as f32
    }
}

#[derive(Debug, Default)]
pub struct StereoAccumulator {
    microphone: VecDeque<f32>,
    system: VecDeque<f32>,
    microphone_received: usize,
    system_received: usize,
}

impl StereoAccumulator {
    pub fn push(&mut self, block: LegBlock) {
        let received = block.samples.len();
        match block.leg {
            AudioLeg::Microphone => {
                self.microphone_received += received;
                self.microphone.extend(block.samples);
            }
            AudioLeg::System => {
                self.system_received += received;
                self.system.extend(block.samples);
            }
        }
    }

    pub fn drain(&mut self) -> DrainedChunk {
        let paired = self.microphone.len().min(self.system.len());
        let mut interleaved = Vec::with_capacity(paired * 2);
        for _ in 0..paired {
            interleaved.push(
                self.microphone
                    .pop_front()
                    .expect("paired microphone sample"),
            );
            interleaved.push(self.system.pop_front().expect("paired system sample"));
        }
        let microphone_dropped = trim_backlog(&mut self.microphone);
        let system_dropped = trim_backlog(&mut self.system);
        let report = PairingReport {
            microphone: LegReport {
                received_samples: self.microphone_received,
                paired_samples: paired,
                retained_backlog_samples: self.microphone.len(),
                dropped_overflow_samples: microphone_dropped,
            },
            system: LegReport {
                received_samples: self.system_received,
                paired_samples: paired,
                retained_backlog_samples: self.system.len(),
                dropped_overflow_samples: system_dropped,
            },
        };
        self.microphone_received = 0;
        self.system_received = 0;
        DrainedChunk {
            interleaved,
            report,
        }
    }
}

fn trim_backlog(samples: &mut VecDeque<f32>) -> usize {
    let overflow = samples.len().saturating_sub(MAX_RETAINED_SAMPLES);
    samples.drain(..overflow);
    overflow
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HitGate {
    hits: usize,
}

impl HitGate {
    pub fn observe(&mut self, chunk: &DrainedChunk) -> bool {
        let hit = chunk.rms() > RMS_THRESHOLD;
        if hit {
            self.hits += 1;
        }
        hit
    }

    pub fn hits(&self) -> usize {
        self.hits
    }

    pub fn should_save(&self) -> bool {
        self.hits >= MIN_HITS_FOR_SAVE
    }
}

pub fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(left: &[f32], right: &[f32]) -> DrainedChunk {
        let mut accumulator = StereoAccumulator::default();
        accumulator.push(LegBlock::new(AudioLeg::Microphone, left.to_vec()));
        accumulator.push(LegBlock::new(AudioLeg::System, right.to_vec()));
        accumulator.drain()
    }

    #[test]
    fn retained_tail_pairs_on_next_drain_without_drift() {
        let mut accumulator = StereoAccumulator::default();
        accumulator.push(LegBlock::new(AudioLeg::Microphone, vec![1.0, 2.0, 3.0]));
        accumulator.push(LegBlock::new(AudioLeg::System, vec![10.0, 20.0]));
        let first = accumulator.drain();
        assert_eq!(first.interleaved(), &[1.0, 10.0, 2.0, 20.0]);
        assert_eq!(first.report.microphone.retained_backlog_samples, 1);

        accumulator.push(LegBlock::new(AudioLeg::System, vec![30.0, 40.0]));
        let second = accumulator.drain();
        assert_eq!(second.interleaved(), &[3.0, 30.0]);
        assert_eq!(second.report.system.retained_backlog_samples, 1);
    }

    #[test]
    fn ingest_sanitizes_non_finite_values_once() {
        let block = LegBlock::new(
            AudioLeg::Microphone,
            vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY],
        );
        assert_eq!(block.samples, vec![0.0, 1e10, -1e10]);
    }

    #[test]
    fn rms_and_hit_gate_match_chunk_contract() {
        let mut gate = HitGate::default();
        assert!(!gate.observe(&chunk(&[0.0; 4], &[0.0; 4])));
        assert!(gate.observe(&chunk(&[0.0; 4], &[0.02; 4])));
        assert!(!gate.observe(&chunk(&[0.01; 4], &[0.0; 4])));
        assert!(gate.observe(&chunk(&[0.010_001; 4], &[0.0; 4])));
        assert_eq!(gate.hits(), 2);
    }

    #[test]
    fn empty_chunk_has_zero_rms_and_no_hit() {
        let mut gate = HitGate::default();
        let chunk = StereoAccumulator::default().drain();
        assert_eq!(chunk.rms(), 0.0);
        assert!(!gate.observe(&chunk));
    }

    #[test]
    fn multi_block_chunk_counts_as_one_hit() {
        let mut accumulator = StereoAccumulator::default();
        for _ in 0..4 {
            accumulator.push(LegBlock::new(AudioLeg::Microphone, vec![0.5; BLOCK_SIZE]));
            accumulator.push(LegBlock::new(AudioLeg::System, vec![0.0; BLOCK_SIZE]));
        }
        let mut gate = HitGate::default();
        assert!(gate.observe(&accumulator.drain()));
        assert_eq!(gate.hits(), 1);
    }

    #[test]
    fn hits_accumulate_across_chunks_and_enable_save() {
        let mut gate = HitGate::default();
        for _ in 0..3 {
            assert!(gate.observe(&chunk(&[0.02], &[0.0])));
        }
        assert_eq!(gate.hits(), 3);
        assert!(gate.should_save());
    }

    #[test]
    fn native_conversion_matches_python_pipeline_outputs() {
        let cases = [
            (0.5, 16383),
            (-1.0, -32767),
            (1.5, 32767),
            (-2.0, -32767),
            (f32::NAN, 0),
            (f32::INFINITY, 32767),
        ];
        for (input, expected) in cases {
            assert_eq!(to_i16(input), expected);
        }
    }

    #[test]
    fn healthy_small_tail_is_not_an_imbalance() {
        let mut accumulator = StereoAccumulator::default();
        accumulator.push(LegBlock::new(AudioLeg::Microphone, vec![0.1; 8]));
        accumulator.push(LegBlock::new(AudioLeg::System, vec![0.1; 7]));
        let drained = accumulator.drain();
        assert!(!drained.report.is_imbalanced());
        assert_eq!(drained.report.microphone.retained_backlog_samples, 1);
        assert_eq!(drained.report.microphone.dropped_overflow_samples, 0);
    }

    #[test]
    fn one_sided_sequence_reports_bounded_backlog_and_overflow() {
        let mut accumulator = StereoAccumulator::default();
        accumulator.push(LegBlock::new(
            AudioLeg::Microphone,
            vec![0.1; MAX_RETAINED_SAMPLES + 8],
        ));
        let drained = accumulator.drain();
        assert!(drained.interleaved().is_empty());
        assert!(drained.report.is_imbalanced());
        assert_eq!(
            drained.report.microphone.retained_backlog_samples,
            MAX_RETAINED_SAMPLES
        );
        assert_eq!(drained.report.microphone.dropped_overflow_samples, 8);

        accumulator.push(LegBlock::new(AudioLeg::System, vec![0.2]));
        let next = accumulator.drain();
        assert_eq!(next.interleaved(), &[0.1, 0.2]);
        assert_eq!(
            next.report.microphone.retained_backlog_samples,
            MAX_RETAINED_SAMPLES - 1
        );
    }
}
