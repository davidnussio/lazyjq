//! Single-line text editor used for the jq query.

use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct LineEditor {
    chars: Vec<char>,
    /// Cursor position as a char index in `0..=chars.len()`.
    cursor: usize,
}

impl LineEditor {
    pub fn new(text: &str) -> Self {
        let mut editor = Self::default();
        editor.set_text(text);
        editor
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Replace the text and move the cursor to the end.
    pub fn set_text(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn insert(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn backward(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn forward(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    pub fn move_to_head(&mut self) {
        self.cursor = 0;
    }

    pub fn move_to_tail(&mut self) {
        self.cursor = self.chars.len();
    }

    pub fn erase(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn erase_forward(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn erase_all(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    pub fn erase_to_tail(&mut self) {
        self.chars.truncate(self.cursor);
    }

    fn previous_word_start(&self, breaks: &[char]) -> usize {
        let mut i = self.cursor;
        // Skip the boundaries right before the cursor, then the word itself.
        while i > 0 && breaks.contains(&self.chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && !breaks.contains(&self.chars[i - 1]) {
            i -= 1;
        }
        i
    }

    fn next_word_end(&self, breaks: &[char]) -> usize {
        let len = self.chars.len();
        let mut i = self.cursor;
        while i < len && breaks.contains(&self.chars[i]) {
            i += 1;
        }
        while i < len && !breaks.contains(&self.chars[i]) {
            i += 1;
        }
        i
    }

    pub fn move_to_previous_word(&mut self, breaks: &[char]) {
        self.cursor = self.previous_word_start(breaks);
    }

    pub fn move_to_next_word(&mut self, breaks: &[char]) {
        self.cursor = self.next_word_end(breaks);
    }

    pub fn erase_to_previous_word(&mut self, breaks: &[char]) {
        let start = self.previous_word_start(breaks);
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn erase_to_next_word(&mut self, breaks: &[char]) {
        let end = self.next_word_end(breaks);
        self.chars.drain(self.cursor..end);
    }

    /// The part of the text visible in `width` columns, scrolled so that the
    /// cursor is visible, and the cursor column within it.
    pub fn viewport(&self, width: usize) -> (String, usize) {
        let width = width.max(1);
        let char_width = |c: &char| c.width().unwrap_or(0);

        // Leave one column for the cursor when it sits at the end.
        let mut start = 0;
        let mut cursor_col: usize = self.chars[..self.cursor].iter().map(char_width).sum();
        while cursor_col >= width && start < self.cursor {
            cursor_col -= char_width(&self.chars[start]);
            start += 1;
        }

        let mut visible = String::new();
        let mut used = 0;
        for c in &self.chars[start..] {
            let w = char_width(c);
            if used + w > width {
                break;
            }
            used += w;
            visible.push(*c);
        }
        (visible, cursor_col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BREAKS: [char; 3] = ['.', '|', ' '];

    #[test]
    fn edits_at_cursor() {
        let mut e = LineEditor::new(".users");
        e.backward();
        e.backward();
        e.insert('X');
        assert_eq!(e.text(), ".useXrs");
        e.erase();
        e.erase_forward();
        assert_eq!(e.text(), ".uses");
        e.erase_to_tail();
        assert_eq!(e.text(), ".use");
    }

    #[test]
    fn word_motions() {
        let mut e = LineEditor::new(".users[] | .name");
        e.erase_to_previous_word(&BREAKS);
        assert_eq!(e.text(), ".users[] | .");
        e.move_to_previous_word(&BREAKS);
        assert_eq!(e.cursor, 1);
        e.erase_to_next_word(&BREAKS);
        assert_eq!(e.text(), ". | .");
    }

    #[test]
    fn viewport_follows_cursor() {
        let e = LineEditor::new("abcdefghij");
        assert_eq!(e.viewport(5), ("ghij".to_string(), 4));
        let mut e = LineEditor::new("abcdefghij");
        e.move_to_head();
        assert_eq!(e.viewport(5), ("abcde".to_string(), 0));
    }
}
