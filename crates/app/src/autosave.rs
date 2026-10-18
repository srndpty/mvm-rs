//! 自動保存の時刻。最後の編集から 2 秒、または編集が続いても 30 秒で書く。
//!
//! 時計は呼び出し側が渡す。試験は待たずに時刻だけ進める。

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutosaveSchedule {
    debounce_ms: u64,
    maximum_ms: u64,
    first_edit_ms: Option<u64>,
    last_edit_ms: Option<u64>,
    armed: bool,
    retry_at_ms: Option<u64>,
}

impl AutosaveSchedule {
    pub const DEBOUNCE_MS: u64 = 2_000;
    pub const MAXIMUM_MS: u64 = 30_000;

    pub fn new() -> Self {
        Self {
            debounce_ms: Self::DEBOUNCE_MS,
            maximum_ms: Self::MAXIMUM_MS,
            first_edit_ms: None,
            last_edit_ms: None,
            armed: false,
            retry_at_ms: None,
        }
    }

    pub fn note_edit(&mut self, now_ms: u64) {
        if self.first_edit_ms.is_none() {
            self.first_edit_ms = Some(now_ms);
        }
        self.last_edit_ms = Some(now_ms);
        self.armed = true;
        self.retry_at_ms = None;
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn mark_written(&mut self) {
        self.armed = false;
        self.first_edit_ms = None;
        self.last_edit_ms = None;
        self.retry_at_ms = None;
    }

    /// 書けなかったときは、次の編集が来るまで 30 秒後にやり直す。
    pub fn note_failure(&mut self, now_ms: u64) {
        self.armed = false;
        self.first_edit_ms = None;
        self.last_edit_ms = None;
        self.retry_at_ms = Some(now_ms.saturating_add(self.maximum_ms));
    }

    pub fn due(&self, now_ms: u64) -> bool {
        if self.retry_at_ms.is_some_and(|retry| now_ms >= retry) {
            return true;
        }
        if !self.armed {
            return false;
        }
        let Some(last) = self.last_edit_ms else {
            return false;
        };
        let Some(first) = self.first_edit_ms else {
            return false;
        };
        now_ms.saturating_sub(last) >= self.debounce_ms
            || now_ms.saturating_sub(first) >= self.maximum_ms
    }
}

impl Default for AutosaveSchedule {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_waits_two_seconds_and_the_maximum_fires_while_edits_continue() {
        let mut schedule = AutosaveSchedule::new();
        assert!(!schedule.due(10_000));
        schedule.note_edit(0);
        assert!(!schedule.due(1_999));
        assert!(schedule.due(2_000));

        schedule.note_edit(1_500);
        assert!(!schedule.due(2_000));
        assert!(schedule.due(3_500));

        let mut busy = AutosaveSchedule::new();
        busy.note_edit(0);
        for second in 1..30 {
            busy.note_edit(second * 1_000);
            assert!(!busy.due(second * 1_000));
        }
        assert!(busy.due(30_000));
    }

    #[test]
    fn clearing_or_a_failure_does_not_write_on_the_debounce() {
        let mut schedule = AutosaveSchedule::new();
        schedule.note_edit(0);
        schedule.clear();
        assert!(!schedule.due(100_000));

        schedule.note_edit(0);
        schedule.note_failure(10);
        assert!(!schedule.due(10));
        assert!(!schedule.due(30_009));
        assert!(schedule.due(30_010));
        schedule.note_edit(30_010);
        assert!(!schedule.due(30_010));
        assert!(schedule.due(32_010));
    }
}
