use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    Me,
    Them,
}

impl Speaker {
    pub fn label(self) -> &'static str {
        match self {
            Speaker::Me => "Me  ",
            Speaker::Them => "Them",
        }
    }
}

/// One finalized utterance. `t` = seconds since session start (monotonic anchor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utterance {
    pub t: f64,
    pub speaker: Speaker,
    pub text: String,
}

/// Finalized utterances (chronological) plus at most one in-flight partial per channel.
#[derive(Default)]
pub struct TranscriptStore {
    pub finals: Vec<Utterance>,
    pub partial_me: Option<String>,
    pub partial_them: Option<String>,
}

impl TranscriptStore {
    pub fn set_partial(&mut self, speaker: Speaker, text: String) {
        let slot = match speaker {
            Speaker::Me => &mut self.partial_me,
            Speaker::Them => &mut self.partial_them,
        };
        *slot = if text.is_empty() { None } else { Some(text) };
    }

    /// Finalize an utterance: clears the channel partial, inserts sorted by anchored start
    /// time so the two channels interleave chronologically.
    pub fn push_final(&mut self, utt: Utterance) {
        self.set_partial(utt.speaker, String::new());
        if utt.text.trim().is_empty() {
            return;
        }
        let idx = self
            .finals
            .iter()
            .rposition(|u| u.t <= utt.t)
            .map_or(0, |i| i + 1);
        self.finals.insert(idx, utt);
    }
}
