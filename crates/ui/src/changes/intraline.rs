//! Bounded, cached character changes. Syntax foregrounds stay intact.
use super::*;
use std::{
    ops::Range,
    time::{Duration, Instant},
};

pub(super) type Changes = HashMap<(u32, bool, u32), Vec<Range<usize>>>;

pub(super) fn key(file: u32, line: &DiffLine) -> Option<(u32, bool, u32)> {
    match line.kind {
        LineKind::Del => line.old_no.map(|n| (file, true, n)),
        LineKind::Add => line.new_no.map(|n| (file, false, n)),
        _ => None,
    }
}

fn ranges(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if old.len() + new.len() > 8192 {
        return (Vec::new(), Vec::new());
    }
    let diff = similar::TextDiff::configure()
        .timeout(Duration::from_millis(5))
        .diff_chars(old, new);
    let (mut old_offset, mut new_offset) = (0, 0);
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            similar::ChangeTag::Delete => {
                append_range(&mut removed, old_offset..old_offset + len);
                old_offset += len;
            }
            similar::ChangeTag::Insert => {
                append_range(&mut added, new_offset..new_offset + len);
                new_offset += len;
            }
            similar::ChangeTag::Equal => {
                old_offset += len;
                new_offset += len;
            }
        }
    }
    (removed, added)
}

fn append_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if let Some(previous) = ranges.last_mut()
        && previous.end == range.start
    {
        previous.end = range.end;
    } else {
        ranges.push(range);
    }
}

pub(super) fn collect(files: &[FileDiff]) -> Changes {
    let started = Instant::now();
    let mut output = Changes::new();
    for (file, diff) in files.iter().enumerate() {
        for hunk in &diff.hunks {
            for (left, right) in split_pairs(&hunk.lines) {
                if started.elapsed() > Duration::from_millis(200) {
                    return output;
                }
                let (Some(left), Some(right)) = (left, right) else {
                    continue;
                };
                let (old, new) = (&hunk.lines[left as usize], &hunk.lines[right as usize]);
                if old.kind != LineKind::Del || new.kind != LineKind::Add {
                    continue;
                }
                let (removed, added) = ranges(&old.text, &new.text);
                if let Some(key) = key(file as u32, old) {
                    output.insert(key, removed);
                }
                if let Some(key) = key(file as u32, new) {
                    output.insert(key, added);
                }
            }
        }
    }
    output
}

pub(super) fn background(theme: &Theme, added: bool, strong: bool) -> gpui::Hsla {
    if theme.appearance.is_dark() {
        let color: gpui::Hsla = gpui::rgb(if added { 0x9bb955 } else { 0xf14c4c }).into();
        color.opacity(if strong { 0.34 } else { 0.15 })
    } else {
        (if added {
            theme.diff_add
        } else {
            theme.diff_del
        })
        .opacity(if strong { 0.22 } else { 0.10 })
    }
}

pub(super) fn paint(
    runs: Vec<gpui::TextRun>,
    ranges: &[Range<usize>],
    color: gpui::Hsla,
) -> Vec<gpui::TextRun> {
    if ranges.is_empty() {
        return runs;
    }
    let mut output = Vec::new();
    let mut offset = 0;
    let mut next_range = 0;
    for run in runs {
        let end = offset + run.len;
        let mut cursor = offset;
        while let Some(range) = ranges.get(next_range) {
            if range.end <= cursor {
                next_range += 1;
                continue;
            }
            if range.start >= end {
                break;
            }
            let start = range.start.max(cursor);
            if cursor < start {
                let mut plain = run.clone();
                plain.len = start - cursor;
                output.push(plain);
            }
            let mut part = run.clone();
            cursor = range.end.min(end);
            part.len = cursor - start;
            part.background_color = Some(color);
            output.push(part);
            if cursor == end {
                break;
            }
        }
        if cursor < end {
            let mut plain = run;
            plain.len = end - cursor;
            output.push(plain);
        }
        offset = end;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn highlights_only_changed_utf8_bytes_and_preserves_syntax_runs() {
        let (old, new) = ranges("const value = 'café';", "const value = 'caffè';");
        let old_text = "const value = 'café';";
        let new_text = "const value = 'caffè';";
        assert!(
            old.iter()
                .all(|r| old_text.is_char_boundary(r.start) && old_text.is_char_boundary(r.end))
        );
        assert!(
            new.iter()
                .all(|r| new_text.is_char_boundary(r.start) && new_text.is_char_boundary(r.end))
        );
        assert!(new.iter().all(|r| r.start >= 17 && r.end <= 21));
        let run = gpui::TextRun {
            len: new_text.len(),
            color: gpui::rgb(0xce9178).into(),
            ..Default::default()
        };
        let result = paint(vec![run.clone()], &new, gpui::rgb(0x00ff00).into());
        assert_eq!(result.iter().map(|r| r.len).sum::<usize>(), new_text.len());
        assert!(
            result
                .iter()
                .all(|r| r.color == run.color && r.font == run.font)
        );
        assert!(result.iter().any(|r| r.background_color.is_some()));
        assert!(result.iter().any(|r| r.background_color.is_none()));
    }
    #[test]
    fn skips_very_large_lines_and_identical_text() {
        assert!(ranges("same", "same").0.is_empty());
        assert!(ranges(&"a".repeat(8193), "new").1.is_empty());
    }
}
