//! Byte offsets ↔ LSP positions. LSP counts lines split at `\n`, `\r\n` or `\r`, and columns in
//! **UTF-16 code units** (the protocol default) — so a `♭` is one column, an emoji two.

/// Line starts of one text (byte offsets). Pass the same text back to the lookups.
#[derive(Clone, Debug)]
pub struct LineIndex {
    starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let mut starts = vec![0];
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => starts.push(i + 1),
                b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                    starts.push(i + 2);
                    i += 1;
                }
                b'\r' => starts.push(i + 1),
                _ => {}
            }
            i += 1;
        }
        LineIndex { starts, len: text.len() }
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// The byte offset where `line` starts (clamped to the end of the text).
    pub fn line_start(&self, line: usize) -> usize {
        self.starts.get(line).copied().unwrap_or(self.len)
    }

    /// The 0-based line holding byte `offset`.
    pub fn line_of(&self, offset: usize) -> usize {
        self.starts.partition_point(|&s| s <= offset).saturating_sub(1)
    }

    /// Byte `offset` → (line, UTF-16 column).
    pub fn position(&self, text: &str, offset: usize) -> (u32, u32) {
        let offset = floor_char_boundary(text, offset.min(text.len()));
        let line = self.line_of(offset);
        let col: usize = text[self.starts[line]..offset].chars().map(char::len_utf16).sum();
        (line as u32, col as u32)
    }

    /// (line, UTF-16 column) → byte offset; a column past the end of its line clamps to the line end,
    /// a line past the end to the end of the text.
    pub fn offset(&self, text: &str, line: u32, col: u32) -> usize {
        let Some(&start) = self.starts.get(line as usize) else { return text.len() };
        let end = self.starts.get(line as usize + 1).copied().unwrap_or(text.len());
        let mut units = 0usize;
        for (i, c) in text[start..end].char_indices() {
            if units >= col as usize || c == '\n' || c == '\r' {
                return start + i;
            }
            units += c.len_utf16();
        }
        end // only the last line has no line break to stop at
    }
}

fn floor_char_boundary(text: &str, mut i: usize) -> usize {
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_utf16_and_every_line_ending() {
        let text = "a♭c\r\nx😀y\rz\nlast";
        let li = LineIndex::new(text);
        assert_eq!(li.line_count(), 4);
        let at = |s: &str| text.find(s).unwrap();
        assert_eq!(li.position(text, at("c")), (0, 2)); // ♭ is one UTF-16 unit
        assert_eq!(li.position(text, at("y")), (1, 3)); // 😀 is two
        assert_eq!(li.position(text, at("z")), (2, 0));
        assert_eq!(li.position(text, at("last")), (3, 0));
        for s in ["a", "c", "x", "y", "z", "last"] {
            let (l, c) = li.position(text, at(s));
            assert_eq!(li.offset(text, l, c), at(s), "{s}");
        }
        // Past the end of a line clamps before its line break; past the last line, the end.
        assert_eq!(li.offset(text, 0, 99), at("\r\n"));
        assert_eq!(li.offset(text, 1, 99), at("\rz"));
        assert_eq!(li.offset(text, 3, 99), text.len());
        assert_eq!(li.offset(text, 9, 0), text.len());
    }
}
