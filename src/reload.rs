use std::{
    fs,
    path::Path,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, PartialEq, Eq)]
/// A cheap fingerprint; reading a half-written PDF would disrupt rendering.
struct Signature {
    len: u64,
    modified: Option<SystemTime>,
}

pub struct Watch {
    last: Option<Signature>,
    pending: Option<(Signature, Instant)>,
}

impl Watch {
    pub fn new(path: &Path) -> Self {
        Self {
            last: signature(path),
            pending: None,
        }
    }
    pub fn changed(&mut self, path: &Path) -> bool {
        // LaTeX tools may replace the PDF through several intermediate states.
        // Report a change only after its size and modification time remain
        // stable for 100ms; missing files are ignored until they reappear.
        let current = signature(path);
        if current == self.last {
            self.pending = None;
            return false;
        }
        let Some(current) = current else {
            return false;
        };
        match &self.pending {
            Some((candidate, since))
                if candidate == &current && since.elapsed() >= Duration::from_millis(100) =>
            {
                self.last = Some(current);
                self.pending = None;
                true
            }
            Some((candidate, _)) if candidate == &current => false,
            _ => {
                self.pending = Some((current, Instant::now()));
                false
            }
        }
    }
}

fn signature(path: &Path) -> Option<Signature> {
    let m = fs::metadata(path).ok()?;
    Some(Signature {
        len: m.len(),
        modified: m.modified().ok(),
    })
}
