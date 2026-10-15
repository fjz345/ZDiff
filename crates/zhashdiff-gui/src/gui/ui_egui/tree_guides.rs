use zhashdiff::fs::FsNodeDepth;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guide {
    Blank,
    Through,
    /// The folder's last child on this side: the line stops at the middle of the row.
    End,
}

/// Guide lines of one side of the drawn rows. Row `i`'s slice holds one guide per ancestor folder
/// below the root, outermost first: index `k` is the line of the ancestor at depth `k + 1`.
#[derive(Default)]
pub struct TreeGuides {
    guides: Vec<Guide>,
    starts: Vec<usize>,
}

impl TreeGuides {
    pub fn row(&self, i: usize) -> &[Guide] {
        &self.guides[self.starts[i]..self.starts[i + 1]]
    }
}

/// `rows`: per drawn row in drawn order, its depth and whether it exists on this side.
pub fn tree_guides(rows: &[(FsNodeDepth, bool)]) -> TreeGuides {
    let mut starts = Vec::with_capacity(rows.len() + 1);
    let mut total = 0;
    starts.push(0);
    for &(depth, _) in rows {
        total += (depth as usize).saturating_sub(1);
        starts.push(total);
    }
    let mut guides = vec![Guide::Blank; total];

    // Bottom-up: `has_later[d]` is whether a later row on this side is a child of the open folder
    // at depth `d`. The root's line (depth 0) is tracked but not drawn: the root row never is.
    let mut has_later: Vec<bool> = Vec::new();
    for (i, &(depth, present)) in rows.iter().enumerate().rev() {
        let depth = depth as usize;
        has_later.resize(depth, false);
        let row = &mut guides[starts[i]..starts[i + 1]];
        for (slot, &later) in row.iter_mut().zip(has_later.iter().skip(1)) {
            if later {
                *slot = Guide::Through;
            }
        }
        if present && depth > 0 {
            if depth > 1 && !has_later[depth - 1] {
                row[depth - 2] = Guide::End;
            }
            has_later[depth - 1] = true;
        }
    }

    TreeGuides { guides, starts }
}

#[cfg(test)]
mod tests {
    use super::{Guide, tree_guides};
    use zhashdiff::fs::FsNodeDepth;

    fn side(rows: &[(&str, bool, bool)], right: bool) -> Vec<String> {
        let input: Vec<(FsNodeDepth, bool)> = rows
            .iter()
            .map(|&(rel_path, on_left, on_right)| {
                let depth = rel_path.split('/').filter(|c| !c.is_empty()).count();
                (depth as _, if right { on_right } else { on_left })
            })
            .collect();
        let guides = tree_guides(&input);
        (0..rows.len())
            .map(|i| {
                guides
                    .row(i)
                    .iter()
                    .map(|guide| match guide {
                        Guide::Blank => ' ',
                        Guide::Through => '│',
                        Guide::End => '╵',
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn guides_follow_each_sides_tree_and_end_at_its_last_child() {
        let rows = [
            ("a/", true, true),
            ("a/b/", true, true),
            ("a/b/c.txt", true, true),
            ("a/b/d.txt", false, true),
            ("a/b/e.txt", true, false),
            ("a/f.txt", true, true),
            ("a/g.txt", false, true),
            ("h/", true, false),
            ("h/i/", true, false),
            ("h/i/j.txt", true, false),
            ("k/", true, true),
            ("l.txt", true, true),
        ];
        let left = ["", "│", "││", "││", "│╵", "╵", " ", "", "╵", " ╵", "", ""];
        let right = ["", "│", "││", "│╵", "│ ", "│", "╵", "", " ", "  ", "", ""];
        assert_eq!(side(&rows, false), left);
        assert_eq!(side(&rows, true), right);
    }
}
