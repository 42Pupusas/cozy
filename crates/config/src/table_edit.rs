//! [`TableEdit`] -- sets keys inside one `[table]` of a TOML document,
//! leaving every other line, comment and blank as the user wrote it.

pub struct TableEdit {
    lines: Vec<String>,
}

impl TableEdit {
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
        }
    }

    #[must_use]
    pub fn text(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }

    /// Set `key = value` in `[table]`: replaced where the key already is,
    /// otherwise added after the table's last line, adding the table at the
    /// end when the document has none.
    pub fn set(&mut self, table: &str, key: &str, value: &str) {
        let line = format!("{key} = {value}");
        let Some(header) = self.header(table) else {
            if self
                .lines
                .last()
                .is_some_and(|last| !last.trim().is_empty())
            {
                self.lines.push(String::new());
            }
            self.lines.push(format!("[{table}]"));
            self.lines.push(line);
            return;
        };
        let end = self.section_end(header);
        if let Some(at) = (header + 1..end).find(|&i| Self::names(&self.lines[i], key)) {
            self.lines[at] = line;
            return;
        }
        let last = (header..end)
            .rev()
            .find(|&i| !self.lines[i].trim().is_empty())
            .unwrap_or(header);
        self.lines.insert(last + 1, line);
    }

    fn header(&self, table: &str) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| Self::table_name(line) == Some(table))
    }

    fn section_end(&self, header: usize) -> usize {
        (header + 1..self.lines.len())
            .find(|&i| self.lines[i].trim_start().starts_with('['))
            .unwrap_or(self.lines.len())
    }

    fn table_name(line: &str) -> Option<&str> {
        let line = line.trim();
        let inner = line.strip_prefix('[')?;
        if inner.starts_with('[') {
            return None;
        }
        let close = inner.find(']')?;
        let rest = inner[close + 1..].trim_start();
        (rest.is_empty() || rest.starts_with('#')).then(|| inner[..close].trim())
    }

    fn names(line: &str, key: &str) -> bool {
        line.trim_start()
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    }
}

#[cfg(test)]
mod tests {
    use super::TableEdit;

    #[test]
    fn an_existing_key_is_replaced_in_place() {
        let mut edit = TableEdit::new("[files]\n# note\ncard_x = 4 # old\ndir = \"#fff\"\n");
        edit.set("files", "card_x", "90");
        assert_eq!(
            edit.text(),
            "[files]\n# note\ncard_x = 90\ndir = \"#fff\"\n"
        );
    }

    #[test]
    fn a_new_key_goes_after_the_tables_last_line_before_the_blank_gap() {
        let mut edit = TableEdit::new("[files]\ndir = \"#fff\"\n\n[canvas]\ngrid = 8\n");
        edit.set("files", "card_y", "12");
        assert_eq!(
            edit.text(),
            "[files]\ndir = \"#fff\"\ncard_y = 12\n\n[canvas]\ngrid = 8\n"
        );
    }

    #[test]
    fn a_missing_table_is_added_at_the_end() {
        let mut edit = TableEdit::new("[font]\nsize = 14\n");
        edit.set("files", "card_x", "1");
        assert_eq!(edit.text(), "[font]\nsize = 14\n\n[files]\ncard_x = 1\n");
    }

    #[test]
    fn an_empty_document_gets_just_the_table() {
        let mut edit = TableEdit::new("");
        edit.set("files", "card_x", "1");
        assert_eq!(edit.text(), "[files]\ncard_x = 1\n");
    }

    #[test]
    fn a_key_in_another_table_or_a_longer_key_is_left_alone() {
        let mut edit = TableEdit::new("[canvas]\ncard_x = 3\n[files]\ncard_xy = 5\n");
        edit.set("files", "card_x", "7");
        assert_eq!(
            edit.text(),
            "[canvas]\ncard_x = 3\n[files]\ncard_xy = 5\ncard_x = 7\n"
        );
    }

    #[test]
    fn array_tables_and_dotted_headers_are_not_the_table() {
        let mut edit = TableEdit::new("[[files]]\na = 1\n[files.sub]\nb = 2\n[ files ] # here\n");
        edit.set("files", "card_x", "7");
        assert_eq!(
            edit.text(),
            "[[files]]\na = 1\n[files.sub]\nb = 2\n[ files ] # here\ncard_x = 7\n"
        );
    }
}
