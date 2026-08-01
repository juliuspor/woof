use async_trait::async_trait;
use woof_llm::CancellationToken;

use crate::AudioError;

pub const TRANSCRIPTION_SAMPLE_RATE: u32 = 24_000;
pub const TRANSCRIPTION_CHANNELS: u16 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioFrame {
    samples: Vec<i16>,
}

impl AudioFrame {
    pub fn new(samples: Vec<i16>) -> Self {
        Self { samples }
    }

    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    pub fn into_samples(self) -> Vec<i16> {
        self.samples
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn normalized_level(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mean_square = self
            .samples
            .iter()
            .map(|sample| {
                let normalized = f64::from(*sample) / f64::from(i16::MAX);
                normalized * normalized
            })
            .sum::<f64>()
            / self.samples.len() as f64;
        mean_square.sqrt().clamp(0.0, 1.0) as f32
    }
}

#[async_trait]
pub trait AudioSource: Send {
    /// Returns the next 24 kHz, mono PCM16 frame.
    ///
    /// `Ok(None)` is a graceful end-of-input and causes the session to commit
    /// its pending audio. Cancellation must return promptly.
    async fn next_frame(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<AudioFrame>, AudioError>;

    /// Stops native capture without committing any additional audio.
    ///
    /// The default is sufficient for finite and synthetic sources.
    fn stop(&mut self) {}
}

/// Stateful linear resampler for mono floating-point input.
///
/// State is retained across calls so Core Audio callback boundaries cannot
/// introduce gaps or duplicate boundary samples.
#[derive(Clone, Debug)]
pub struct Pcm16Resampler {
    step: f64,
    next_output_position: f64,
    input_position: u64,
    previous: Option<f32>,
}

impl Pcm16Resampler {
    pub fn new(input_sample_rate: u32) -> Self {
        assert!(input_sample_rate > 0, "input sample rate must be non-zero");
        Self {
            step: f64::from(input_sample_rate) / f64::from(TRANSCRIPTION_SAMPLE_RATE),
            next_output_position: 0.0,
            input_position: 0,
            previous: None,
        }
    }

    pub fn process_mono<I>(&mut self, input: I) -> Vec<i16>
    where
        I: IntoIterator<Item = f32>,
    {
        let mut output = Vec::new();
        for current in input {
            let current = sanitize_sample(current);
            let current_position = self.input_position as f64;
            if let Some(previous) = self.previous {
                let previous_position = current_position - 1.0;
                while self.next_output_position <= current_position {
                    if self.next_output_position >= previous_position {
                        let fraction =
                            (self.next_output_position - previous_position).clamp(0.0, 1.0) as f32;
                        let interpolated = previous + ((current - previous) * fraction);
                        output.push(float_to_pcm16(interpolated));
                    }
                    self.next_output_position += self.step;
                }
            } else {
                while self.next_output_position <= current_position {
                    output.push(float_to_pcm16(current));
                    self.next_output_position += self.step;
                }
            }
            self.previous = Some(current);
            self.input_position = self.input_position.saturating_add(1);
        }
        output
    }
}

fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn float_to_pcm16(sample: f32) -> i16 {
    let sample = sanitize_sample(sample);
    if sample <= -1.0 {
        i16::MIN
    } else {
        (sample * f32::from(i16::MAX)).round() as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_level_is_root_mean_square_and_bounded() {
        assert_eq!(AudioFrame::new(Vec::new()).normalized_level(), 0.0);
        assert_eq!(AudioFrame::new(vec![0, 0]).normalized_level(), 0.0);
        let full_scale = AudioFrame::new(vec![i16::MAX]).normalized_level();
        assert!((0.99..=1.0).contains(&full_scale));
    }

    #[test]
    fn resamples_48_khz_across_callback_boundaries() {
        let mut resampler = Pcm16Resampler::new(48_000);
        let first = resampler.process_mono([0.0, 0.1, 0.2]);
        let second = resampler.process_mono([0.3, 0.4]);
        assert_eq!(
            [first, second].concat(),
            vec![0, float_to_pcm16(0.2), float_to_pcm16(0.4)]
        );
    }

    #[test]
    fn preserves_24_khz_samples_and_sanitizes_non_finite_input() {
        let mut resampler = Pcm16Resampler::new(TRANSCRIPTION_SAMPLE_RATE);
        assert_eq!(
            resampler.process_mono([1.0, -1.0, f32::NAN]),
            vec![i16::MAX, i16::MIN, 0]
        );
    }
}
