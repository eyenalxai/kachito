//! The toast stack: transient and persistent notices over the shell.

/// How long a transient toast stays on screen, in seconds.
pub(crate) const TRANSIENT_SECONDS: f64 = 4.0;
/// The most toasts on screen at once. A new one drops the oldest.
pub(crate) const MAX_VISIBLE: usize = 3;

/// What a toast is telling the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Danger,
    Success,
    Neutral,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Toast {
    pub id: u64,
    pub title: String,
    /// The line under the title, for the detail a failure carries.
    pub detail: Option<String>,
    pub tone: Tone,
    /// A persistent toast stays until its close control is pressed.
    pub persistent: bool,
    /// The app clock time at which a transient toast goes, or `None` while it persists.
    pub expires_at: Option<f64>,
}

#[derive(Debug, Default)]
pub(crate) struct Toasts {
    entries: Vec<Toast>,
    next_id: u64,
}

impl Toasts {
    /// Raises one toast. An identical title and detail on screen is refreshed in place
    /// instead of stacking a duplicate, and the oldest toast leaves when the cap is
    /// reached. Returns whether the view changed.
    pub fn push(
        &mut self,
        title: impl Into<String>,
        detail: Option<String>,
        tone: Tone,
        persistent: bool,
        now: f64,
    ) -> bool {
        let title = title.into();
        let detail = detail.filter(|detail| !detail.trim().is_empty());
        let expires_at = (!persistent).then_some(now + TRANSIENT_SECONDS);
        if let Some(toast) = self
            .entries
            .iter_mut()
            .find(|toast| toast.title == title && toast.detail == detail)
        {
            let changed = toast.tone != tone || toast.persistent != persistent;
            toast.tone = tone;
            toast.persistent = persistent;
            toast.expires_at = expires_at;
            return changed;
        }
        self.next_id += 1;
        self.entries.push(Toast {
            id: self.next_id,
            title,
            detail,
            tone,
            persistent,
            expires_at,
        });
        if self.entries.len() > MAX_VISIBLE {
            self.entries.remove(0);
        }
        true
    }

    /// Removes the toasts whose time has come. Returns whether any left.
    pub fn prune(&mut self, now: f64) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|toast| toast.expires_at.is_none_or(|expires_at| expires_at > now));
        self.entries.len() != before
    }

    /// The earliest time a transient toast goes, for the next poll wake.
    pub fn next_deadline(&self) -> Option<f64> {
        self.entries
            .iter()
            .filter_map(|toast| toast.expires_at)
            .min_by(f64::total_cmp)
    }

    pub fn dismiss(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|toast| toast.id != id);
        self.entries.len() != before
    }

    /// Clears the stack, as a successful open does.
    pub fn clear(&mut self) -> bool {
        let changed = !self.entries.is_empty();
        self.entries.clear();
        changed
    }

    pub fn entries(&self) -> &[Toast] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A detail as a sentence: first letter capital, a closing period when it has none.
/// A leading path or command keeps its own letters.
pub(crate) fn sentence_case(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut chars = text.chars();
    let mut sentence = String::with_capacity(text.len() + 1);
    if let Some(first) = chars.next() {
        sentence.extend(first.to_uppercase());
    }
    sentence.push_str(chars.as_str());
    if !sentence.ends_with(['.', '!', '?', ':', ';', '…']) {
        sentence.push('.');
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(toasts: &Toasts) -> Vec<&str> {
        toasts
            .entries()
            .iter()
            .map(|toast| toast.title.as_str())
            .collect()
    }

    #[test]
    fn a_new_toast_is_transient_or_persistent() {
        let mut toasts = Toasts::default();
        assert!(toasts.push(
            "Seek took too long",
            Some("Playback continues.".into()),
            Tone::Danger,
            false,
            10.0
        ));
        assert!(toasts.push("Export failed", None, Tone::Danger, true, 10.0));
        assert_eq!(toasts.entries()[0].expires_at, Some(14.0));
        assert_eq!(toasts.entries()[1].expires_at, None);
    }

    #[test]
    fn a_duplicate_title_and_detail_refresh_in_place() {
        let mut toasts = Toasts::default();
        toasts.push(
            "Export failed",
            Some("ffmpeg exited.".into()),
            Tone::Danger,
            true,
            10.0,
        );
        toasts.push(
            "Export failed",
            Some("ffmpeg exited.".into()),
            Tone::Danger,
            true,
            20.0,
        );
        assert_eq!(toasts.entries().len(), 1);
        assert_eq!(messages(&toasts), vec!["Export failed"]);
    }

    #[test]
    fn the_same_title_with_a_new_detail_stacks() {
        let mut toasts = Toasts::default();
        toasts.push(
            "Export failed",
            Some("ffmpeg exited.".into()),
            Tone::Danger,
            true,
            0.0,
        );
        toasts.push(
            "Export failed",
            Some("the file was in use.".into()),
            Tone::Danger,
            true,
            0.0,
        );
        assert_eq!(toasts.entries().len(), 2);
    }

    #[test]
    fn a_refresh_resets_a_transient_timer() {
        let mut toasts = Toasts::default();
        toasts.push("Seek took too long", None, Tone::Danger, false, 10.0);
        toasts.push("Seek took too long", None, Tone::Danger, false, 13.0);
        assert_eq!(toasts.entries()[0].expires_at, Some(17.0));
        assert!(!toasts.prune(16.0));
        assert!(toasts.prune(17.1));
        assert!(toasts.is_empty());
    }

    #[test]
    fn the_cap_drops_the_oldest_toast() {
        let mut toasts = Toasts::default();
        for (index, message) in ["one", "two", "three", "four"].into_iter().enumerate() {
            toasts.push(message, None, Tone::Danger, true, index as f64);
        }
        assert_eq!(toasts.entries().len(), MAX_VISIBLE);
        assert_eq!(messages(&toasts), vec!["two", "three", "four"]);
    }

    #[test]
    fn the_next_deadline_is_the_earliest_transient() {
        let mut toasts = Toasts::default();
        assert_eq!(toasts.next_deadline(), None);
        toasts.push("persistent", None, Tone::Danger, true, 0.0);
        assert_eq!(toasts.next_deadline(), None);
        toasts.push("later", None, Tone::Danger, false, 2.0);
        toasts.push("sooner", None, Tone::Danger, false, 1.0);
        assert_eq!(toasts.next_deadline(), Some(5.0));
    }

    #[test]
    fn a_dismiss_removes_one_toast() {
        let mut toasts = Toasts::default();
        toasts.push("first", None, Tone::Danger, true, 0.0);
        toasts.push("second", None, Tone::Danger, true, 0.0);
        let id = toasts.entries()[0].id;
        assert!(toasts.dismiss(id));
        assert_eq!(messages(&toasts), vec!["second"]);
        assert!(!toasts.dismiss(id));
    }

    #[test]
    fn a_clear_empties_the_stack() {
        let mut toasts = Toasts::default();
        toasts.push("first", None, Tone::Danger, true, 0.0);
        assert!(toasts.clear());
        assert!(toasts.is_empty());
        assert!(!toasts.clear());
    }

    #[test]
    fn a_duplicate_that_changes_tone_or_persistence_rebuilds() {
        let mut toasts = Toasts::default();
        toasts.push("Export failed", None, Tone::Danger, false, 0.0);
        assert!(toasts.push("Export failed", None, Tone::Danger, true, 0.0));
        assert!(toasts.entries()[0].persistent);
        assert!(!toasts.push("Export failed", None, Tone::Danger, true, 1.0));
    }

    #[test]
    fn an_empty_detail_is_dropped() {
        let mut toasts = Toasts::default();
        toasts.push("Export failed", Some("   ".into()), Tone::Danger, true, 0.0);
        assert_eq!(toasts.entries()[0].detail, None);
    }

    #[test]
    fn a_detail_gets_a_capital_and_a_period() {
        assert_eq!(
            sentence_case("unrecognized file format"),
            "Unrecognized file format."
        );
        assert_eq!(
            sentence_case("The decoder stopped."),
            "The decoder stopped."
        );
        assert_eq!(
            sentence_case("set volume 40: rejected"),
            "Set volume 40: rejected."
        );
        assert_eq!(
            sentence_case("/tmp/clip.mkv is missing"),
            "/tmp/clip.mkv is missing."
        );
        assert_eq!(sentence_case(""), "");
    }
}
