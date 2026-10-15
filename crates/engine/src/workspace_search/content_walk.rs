//! On-demand, bounded content scan for explicitly including ignored files.
//! Keep the normal shared index small; an ignored-file search retains no index.
use super::{CONTENT_TIME_BUDGET, ContentMatch, ContentSearch, WorkspaceSearchError};
use std::{
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use zeron_proto::WorkspaceContentSearchOptions;

const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

pub(super) fn search(
    root: &Path,
    query: &str,
    pattern: &str,
    limit: usize,
    per_file: usize,
    options: &WorkspaceContentSearchOptions,
    cancel: Arc<AtomicBool>,
) -> Result<ContentSearch, WorkspaceSearchError> {
    let compile_glob = |pattern: Option<String>| -> Result<_, WorkspaceSearchError> {
        pattern
            .map(|pattern| {
                globset::GlobBuilder::new(&pattern)
                    .literal_separator(true)
                    .build()
                    .map(|glob| glob.compile_matcher())
                    .map_err(|error| {
                        WorkspaceSearchError::InvalidQuery(format!("Invalid file filter: {error}"))
                    })
            })
            .transpose()
    };
    let include = compile_glob(super::content_glob(&options.include)?)?;
    let exclude = compile_glob(super::content_glob(&options.exclude)?)?;
    let folder = super::content_folder(&options.folder)?;
    let expression = if options.use_regex || options.whole_word {
        pattern.to_owned()
    } else {
        regex::escape(query)
    };
    let matcher = regex::RegexBuilder::new(&expression)
        .case_insensitive(
            !options
                .match_case
                .unwrap_or_else(|| query.chars().any(char::is_uppercase)),
        )
        .size_limit(2 * 1024 * 1024)
        .build()
        .map_err(|error| {
            WorkspaceSearchError::InvalidQuery(format!("Invalid regular expression: {error}"))
        })?;
    let mut result = ContentSearch::default();
    if limit == 0 || per_file == 0 || query.trim().is_empty() {
        return Ok(result);
    }
    let scan_root = folder
        .as_ref()
        .map_or_else(|| root.to_path_buf(), |folder| root.join(folder));
    if let Some(folder) = &folder
        && super::escapes_through_symlink(root, folder, false)
    {
        return Ok(result);
    }
    if !scan_root.is_dir() {
        return Ok(result);
    }
    let started = Instant::now();
    let expired = || cancel.load(Ordering::Relaxed) || started.elapsed() >= CONTENT_TIME_BUDGET;
    let mut walk = ignore::WalkBuilder::new(&scan_root);
    walk.hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .parents(false)
        .follow_links(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| entry.file_name() != ".git");
    for entry in walk.build() {
        if expired() {
            result.truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(relative) = relative.to_str() else {
            continue;
        };
        let path = relative.replace(std::path::MAIN_SEPARATOR, "/");
        if crate::workspace_files::is_internal_temp_wire_path(&path)
            || include.as_ref().is_some_and(|glob| !glob.is_match(&path))
            || exclude.as_ref().is_some_and(|glob| glob.is_match(&path))
            || entry
                .metadata()
                .map_or(true, |meta| meta.len() > MAX_FILE_BYTES)
        {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_FILE_BYTES
            || bytes.contains(&0)
        {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let mut file_matches = 0;
        for (line, text) in text.lines().enumerate() {
            if expired() {
                result.truncated = true;
                break;
            }
            let Some(first) = matcher.find(text) else {
                continue;
            };
            if file_matches >= per_file || result.matches.len() >= limit {
                result.truncated = true;
                break;
            }
            let indent = text.len() - text.trim_start().len();
            let mut start = if first.start() > 432 {
                first.start() - 80
            } else {
                indent.min(first.start())
            };
            while !text.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = (start + 512).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let ranges = matcher
                .find_iter(text)
                .take_while(|matched| matched.start() < end)
                .filter(|matched| matched.end() > start)
                .map(|matched| {
                    (
                        matched.start().saturating_sub(start) as u32,
                        (matched.end().min(end) - start) as u32,
                    )
                })
                .collect();
            result.matches.push(ContentMatch {
                path: path.clone(),
                line: line as u64 + 1,
                column: first.start() as u64,
                preview: text[start..end].to_owned(),
                ranges,
            });
            file_matches += 1;
        }
        if result.matches.len() >= limit || expired() {
            result.truncated = true;
            break;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_scan_reports_limits_regex_offsets_and_cancels_per_request() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let text = format!("{} 😀 Needle\nneedle\nneedle\n", "a".repeat(700));
        std::fs::write(root.join("note.txt"), &text).unwrap();
        let search = crate::workspace_search::WorkspaceSearch::new();
        let controls = WorkspaceContentSearchOptions {
            include_ignored: true,
            use_regex: true,
            match_case: Some(false),
            ..Default::default()
        };
        let cancelled = search
            .search_content_cancellable(
                &root,
                "needl[e]",
                2,
                2,
                &controls,
                Arc::new(AtomicBool::new(true)),
            )
            .unwrap();
        assert!(cancelled.truncated && cancelled.matches.is_empty());
        let found = search
            .search_content_with_options(&root, "needl[e]", 2, 2, &controls)
            .unwrap();
        assert_eq!(found.matches.len(), 2);
        assert!(found.truncated);
        assert_eq!(found.matches[0].column, 706);
        let preview = &found.matches[0];
        assert!(preview.preview.len() <= 512);
        let (start, end) = preview.ranges[0];
        assert_eq!(&preview.preview[start as usize..end as usize], "Needle");
    }
}
