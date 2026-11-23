#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ClampedCursor {
    #[serde(skip)]
    cursor: usize,
    #[serde(skip)]
    prev_cursor: usize,
    #[serde(skip)]
    max: usize,
}
impl ClampedCursor {
    pub fn new(cursor: usize, max: usize) -> Self {
        Self {
            cursor,
            prev_cursor: cursor,
            max,
        }
    }
    pub fn inc(&mut self) {
        self.prev_cursor = self.cursor;
        self.cursor = (self.cursor + 1).min(self.max);
        log::trace!("ClampedCursor++ @{}", self.cursor);
    }
    pub fn dec(&mut self) {
        self.prev_cursor = self.cursor;
        self.cursor = self.cursor.saturating_sub(1);
        log::trace!("ClampedCursor-- @{}", self.cursor);
    }
    pub fn set(&mut self, new_cursor: usize) {
        self.prev_cursor = self.cursor;
        self.cursor = new_cursor;
    }
    pub fn invalidate_ack(&mut self) {
        assert_ne!(
            self.cursor,
            usize::MAX,
            "Cursor should not be MAX when invalidating"
        );
        self.prev_cursor = usize::MAX;
    }
    pub fn set_max(&mut self, new_max: usize) {
        self.max = new_max;
    }
    pub fn get_max(&self) -> usize {
        self.max
    }
    pub fn get(&self) -> usize {
        self.cursor
    }
    pub fn get_prev(&self) -> usize {
        self.cursor
    }
    pub fn diff(&self) -> isize {
        if self.cursor >= self.prev_cursor {
            (self.cursor - self.prev_cursor) as isize
        } else {
            -((self.prev_cursor - self.cursor) as isize)
        }
    }
    pub fn has_changed(&self) -> bool {
        self.diff() != 0
    }
    pub fn ack_change(&mut self) {
        self.prev_cursor = self.cursor;
    }
}
