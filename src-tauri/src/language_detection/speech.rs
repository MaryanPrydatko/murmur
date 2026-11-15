//! The speech the language detector hears: silence cut out by VAD, so a
//! window of N seconds is N seconds of speech.

use anyhow::Result;
use log::warn;

use crate::audio_toolkit::vad::{VadFrame, VoiceActivityDetector};

/// The first `max_samples` of speech in `audio`, as `vad` keeps it.
pub fn speech_head(
    vad: &mut dyn VoiceActivityDetector,
    audio: &[f32],
    max_samples: usize,
) -> Result<Vec<f32>> {
    let mut speech = Vec::with_capacity(max_samples.min(audio.len()));
    for frame in audio.chunks_exact(vad.frame_samples()) {
        if let VadFrame::Speech(kept) = vad.push_frame(frame)? {
            speech.extend_from_slice(kept);
            if speech.len() >= max_samples {
                break;
            }
        }
    }
    speech.truncate(max_samples);
    Ok(speech)
}

/// [`speech_head`] over audio that arrives a piece at a time.
pub struct SpeechHead<V> {
    vad: V,
    /// The end of the last piece that didn't fill a frame.
    carry: Vec<f32>,
    speech: Vec<f32>,
    max_samples: usize,
}

impl<V: VoiceActivityDetector> SpeechHead<V> {
    pub fn new(vad: V, max_samples: usize) -> Self {
        Self {
            vad,
            carry: Vec::new(),
            speech: Vec::new(),
            max_samples,
        }
    }

    pub fn is_full(&self) -> bool {
        self.speech.len() >= self.max_samples
    }

    pub fn push(&mut self, piece: &[f32]) -> Result<()> {
        if self.is_full() {
            return Ok(());
        }
        let mut audio = std::mem::take(&mut self.carry);
        audio.extend_from_slice(piece);
        let whole = audio.len() / self.vad.frame_samples() * self.vad.frame_samples();
        let left = self.max_samples - self.speech.len();
        let speech = speech_head(&mut self.vad, &audio[..whole], left)?;
        self.speech.extend(speech);
        self.carry = audio.split_off(whole);
        Ok(())
    }

    pub fn into_speech(self) -> Vec<f32> {
        self.speech
    }
}

/// Takes pieces from `pieces` into `head` until it holds its window of
/// speech, `max_pieces` are taken or the VAD fails, and returns them to be
/// used again.
pub fn gather_head<V: VoiceActivityDetector>(
    pieces: &mut impl Iterator<Item = Vec<f32>>,
    head: &mut SpeechHead<V>,
    max_pieces: usize,
) -> Vec<Vec<f32>> {
    let mut taken = Vec::new();
    while !head.is_full() && taken.len() < max_pieces {
        let Some(piece) = pieces.next() else {
            break;
        };
        let pushed = head.push(&piece);
        taken.push(piece);
        if let Err(e) = pushed {
            warn!("Language detection: VAD failed: {e:#}");
            break;
        }
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames whose first sample is positive are speech.
    struct SignVad;

    impl VoiceActivityDetector for SignVad {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
            Ok(if frame[0] > 0.0 {
                VadFrame::Speech(frame)
            } else {
                VadFrame::Noise
            })
        }

        fn frame_samples(&self) -> usize {
            2
        }
    }

    #[test]
    fn keeps_only_speech_up_to_the_window() {
        let audio = [0.0, 0.0, 1.0, 2.0, 0.0, 0.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(
            speech_head(&mut SignVad, &audio, 3).unwrap(),
            vec![1.0, 2.0, 3.0]
        );
        assert_eq!(
            speech_head(&mut SignVad, &audio, 100).unwrap(),
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
        );
        assert!(speech_head(&mut SignVad, &[0.0; 6], 4).unwrap().is_empty());
    }

    #[test]
    fn a_silent_first_piece_leaves_the_window_to_the_next() {
        let silent = vec![0.0; 7];
        // Odd lengths: a frame is split across pieces.
        let speech = vec![1.0; 7];
        let mut pieces = vec![silent.clone(), speech.clone(), speech.clone(), speech].into_iter();
        let mut head = SpeechHead::new(SignVad, 10);
        let taken = gather_head(&mut pieces, &mut head, 10);
        assert_eq!(taken.len(), 3, "stops once the window is full");
        assert_eq!(head.into_speech(), vec![1.0; 10]);
        assert_eq!(pieces.len(), 1, "the rest is left to transcribe");
    }

    #[test]
    fn gathering_stops_after_the_most_pieces_it_may_take() {
        let mut pieces = std::iter::repeat_n(vec![0.0; 8], 20);
        let mut head = SpeechHead::new(SignVad, 10);
        assert_eq!(gather_head(&mut pieces, &mut head, 10).len(), 10);
        assert!(head.into_speech().is_empty());
    }
}
