//! SDP helpers shared by the session: the ICE generations of the current
//! remote description and the ufrag of one candidate.

use crate::Candidate;

pub(super) struct RemoteSdp {
    pub(super) ufrags: Vec<String>,
}

impl RemoteSdp {
    pub(super) fn new(sdp: &str) -> Self {
        let mut ufrags = Vec::new();
        for ufrag in sdp
            .lines()
            .filter_map(|line| line.strip_prefix("a=ice-ufrag:"))
        {
            if !ufrags.iter().any(|value| value == ufrag) {
                ufrags.push(ufrag.to_owned());
            }
        }
        Self { ufrags }
    }

    pub(super) fn accepts(&self, candidate: &Candidate) -> bool {
        candidate.ufrag().is_none_or(|ufrag| {
            self.ufrags.is_empty() || self.ufrags.iter().any(|value| value == ufrag)
        })
    }
}

impl Candidate {
    pub(super) fn ufrag(&self) -> Option<&str> {
        self.username_fragment
            .as_deref()
            .or_else(|| candidate_ufrag(&self.candidate))
    }
}

pub(super) fn candidate_ufrag(candidate: &str) -> Option<&str> {
    let mut tokens = candidate.split_ascii_whitespace().skip(8);
    while let (Some(key), Some(value)) = (tokens.next(), tokens.next()) {
        if key == "ufrag" {
            return Some(value);
        }
    }
    None
}
