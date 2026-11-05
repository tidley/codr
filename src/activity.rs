//! Durable attribution of native user input without storing its plaintext.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct UserInput {
    pub id: String,
    pub fingerprint: Option<String>,
    complete: bool,
}

impl UserInput {
    pub fn new(id: String, text: &str) -> Self {
        let text = text.replace("\r\n", "\n");
        Self {
            id,
            complete: true,
            fingerprint: (!text.trim().is_empty())
                .then(|| hex::encode(Sha256::digest(text.trim().as_bytes()))),
        }
    }

    pub fn pending(id: String) -> Self {
        Self {
            id,
            fingerprint: None,
            complete: false,
        }
    }
}

/// Terminal delivery is not native acceptance. Keep an independent, private
/// receipt until an unambiguous new native input has the submitted fingerprint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Submission {
    pub receipt: String,
    before: Option<Vec<String>>,
    fingerprint: Option<String>,
    pub submitted_at: u64,
    pub warned: bool,
}

impl Submission {
    pub fn new(receipt: &str, text: &str, inputs: Option<&[UserInput]>, at: u64) -> Self {
        Self {
            receipt: receipt.into(),
            before: inputs.map(|inputs| inputs.iter().map(|input| input.id.clone()).collect()),
            fingerprint: UserInput::new(String::new(), text).fingerprint,
            submitted_at: at,
            warned: false,
        }
    }

    pub fn overdue(&self, at: u64) -> bool {
        at.saturating_sub(self.submitted_at) >= 60
    }

    pub fn observe(submissions: &mut Vec<Self>, inputs: &[UserInput]) {
        // Match the whole group: one echo must not confirm two equal submissions,
        // nor may two equal native inputs be attributed to one phone receipt.
        let accepted: Vec<_> = submissions
            .iter()
            .enumerate()
            .filter_map(|(index, submission)| {
                let fingerprint = submission.fingerprint.as_ref()?;
                let peers: Vec<_> = submissions
                    .iter()
                    .filter(|other| other.fingerprint.as_ref() == Some(fingerprint))
                    .collect();
                let mut ids = HashSet::new();
                let echoes: Vec<_> = inputs
                    .iter()
                    .filter(|input| {
                        input.complete
                            && input.fingerprint.as_ref() == Some(fingerprint)
                            && peers.iter().any(|peer| {
                                peer.before
                                    .as_ref()
                                    .is_some_and(|before| !before.contains(&input.id))
                            })
                            && ids.insert(input.id.as_str())
                    })
                    .collect();
                if echoes.len() != peers.len() {
                    return None;
                }
                let candidates: Vec<Vec<usize>> = peers
                    .iter()
                    .map(|peer| {
                        echoes
                            .iter()
                            .enumerate()
                            .filter(|(_, input)| {
                                peer.before
                                    .as_ref()
                                    .is_some_and(|before| !before.contains(&input.id))
                            })
                            .map(|(index, _)| index)
                            .collect()
                    })
                    .collect();
                let mut owners = vec![None; echoes.len()];
                (0..peers.len())
                    .all(|peer| {
                        assign_native_input(
                            peer,
                            &candidates,
                            &mut owners,
                            &mut vec![false; echoes.len()],
                        )
                    })
                    .then_some(index)
            })
            .collect();
        let mut index = 0;
        submissions.retain(|_| {
            let keep = !accepted.contains(&index);
            index += 1;
            keep
        });
    }
}

// Each receipt must own a different eligible native message. Reassign an earlier
// match if a later receipt has a narrower baseline (for example, an incomplete echo).
fn assign_native_input(
    receipt: usize,
    candidates: &[Vec<usize>],
    owners: &mut [Option<usize>],
    seen: &mut [bool],
) -> bool {
    for &input in &candidates[receipt] {
        if seen[input] {
            continue;
        }
        seen[input] = true;
        if owners[input]
            .is_none_or(|previous| assign_native_input(previous, candidates, owners, seen))
        {
            owners[input] = Some(receipt);
            return true;
        }
    }
    false
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PhoneInput {
    receipt: String,
    before: Vec<String>,
    fingerprint: Option<String>,
    confirmed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    observed: Option<Vec<String>>,
    phone: Vec<PhoneInput>,
    #[serde(default)]
    unresolved: bool,
}

impl Activity {
    /// First observation is a migration baseline, never evidence of fresh activity.
    pub fn observe(&mut self, inputs: &[UserInput]) -> bool {
        let Some(observed) = &mut self.observed else {
            self.observed = Some(inputs.iter().map(|input| input.id.clone()).collect());
            return false;
        };
        let known: HashSet<_> = observed.iter().cloned().collect();
        self.unresolved = inputs
            .iter()
            .any(|input| !input.complete && !known.contains(&input.id));
        let fresh: Vec<_> = inputs
            .iter()
            .filter(|input| input.complete && !known.contains(&input.id))
            .collect();
        // Group fresh text by fingerprint. A resend produces two phone receipts
        // with the same text and two echoes; matching counts keeps that
        // attributable instead of treating the whole batch as local input.
        let mut by_text: std::collections::BTreeMap<String, Vec<&UserInput>> =
            std::collections::BTreeMap::new();
        let mut local = fresh.iter().any(|input| input.fingerprint.is_none());
        for input in &fresh {
            if let Some(fingerprint) = &input.fingerprint {
                by_text.entry(fingerprint.clone()).or_default().push(input);
            }
        }
        let mut consumed = HashSet::new();
        for (fingerprint, group) in &by_text {
            let candidates: Vec<_> = self
                .phone
                .iter()
                .filter(|phone| {
                    phone.confirmed
                        && phone.fingerprint.as_deref() == Some(fingerprint)
                        && group.iter().all(|input| !phone.before.contains(&input.id))
                })
                .collect();
            if candidates.len() == group.len() {
                consumed.insert(fingerprint.clone());
            } else {
                local = true;
            }
        }
        // A genuine local/ambiguous input still invalidates outstanding attribution.
        if local {
            self.phone.clear();
        } else {
            self.phone.retain(|phone| {
                !phone.confirmed
                    || !phone
                        .fingerprint
                        .as_deref()
                        .is_some_and(|fingerprint| consumed.contains(fingerprint))
            });
        }
        observed.extend(fresh.into_iter().map(|input| input.id.clone()));
        local
    }

    pub fn baseline(&mut self, inputs: &[UserInput]) {
        let _ = self.observe(inputs);
    }

    pub fn unresolved(&self) -> bool {
        self.unresolved
    }

    /// An ended session cannot complete attribution reliably. The caller mutes first.
    pub fn retire(&mut self) {
        self.unresolved = false;
        self.phone.clear();
    }

    pub fn start_phone(&mut self, receipt: &str, text: &str, inputs: &[UserInput]) {
        self.baseline(inputs);
        self.start_phone_current(receipt, text);
    }

    pub fn start_phone_current(&mut self, receipt: &str, text: &str) {
        let before = self.observed.get_or_insert_with(Vec::new).clone();
        self.phone.push(PhoneInput {
            receipt: receipt.into(),
            before,
            fingerprint: UserInput::new(String::new(), text).fingerprint,
            confirmed: false,
        });
    }

    pub fn confirm_phone(&mut self, receipt: &str) {
        for phone in &mut self.phone {
            if phone.receipt == receipt {
                phone.confirmed = true;
            }
        }
    }

    pub fn cancel_phone(&mut self, receipt: &str) {
        self.phone.retain(|phone| phone.receipt != receipt);
    }
}
