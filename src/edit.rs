//! `islet add` and `islet remove`: change the `dependencies:` table of an `islet.yaml` as text, so comments,
//! blank lines and the order of everything else stay as the author wrote them. The caller re-reads the result with
//! the manifest parser before writing it, so an edit can never leave an unreadable file.

use crate::yaml;

/// What to write for the dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Version(String),
    FromMonorepo,
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

fn is_blank_or_comment(l: &str) -> bool {
    let t = l.trim();
    t.is_empty() || t.starts_with('#')
}

/// The key of a `key:` or `key: value` line, without quotes.
fn line_key(l: &str) -> Option<String> {
    let t = l.trim_start();
    if t.starts_with('#') || t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix('"') {
        return rest.find('"').map(|i| rest[..i].to_string());
    }
    if let Some(rest) = t.strip_prefix('\'') {
        return rest.find('\'').map(|i| rest[..i].to_string());
    }
    t.find(':').map(|i| t[..i].to_string())
}

struct Block {
    /// Line of `dependencies:`.
    head: usize,
    /// One past the last line that belongs to the block (trailing blanks and comments excluded).
    end: usize,
    indent: usize,
    /// For each entry: (first line including its comments, key line, key).
    entries: Vec<(usize, usize, String)>,
}

fn find_block(lines: &[String]) -> Option<Block> {
    let head = lines.iter().position(|l| {
        let t = l.split(" #").next().unwrap_or(l).trim_end();
        t == "dependencies:" || t == "dependencies: {}"
    })?;
    let mut end = head + 1;
    let mut indent = 2;
    let mut seen_child = false;
    let mut i = head + 1;
    while i < lines.len() {
        let l = &lines[i];
        if l.trim().is_empty() {
            i += 1;
            continue;
        }
        if indent_of(l) == 0 {
            break;
        }
        if !seen_child && !is_blank_or_comment(l) {
            indent = indent_of(l);
            seen_child = true;
        }
        if !l.trim_start().starts_with('#') {
            end = i + 1;
        }
        i += 1;
    }
    let mut entries: Vec<(usize, usize, String)> = Vec::new();
    for k in head + 1..end {
        let l = &lines[k];
        if indent_of(l) == indent && !is_blank_or_comment(l) {
            if let Some(key) = line_key(l) {
                let mut first = k;
                while first > head + 1
                    && lines[first - 1].trim_start().starts_with('#')
                    && indent_of(&lines[first - 1]) == indent
                {
                    first -= 1;
                }
                entries.push((first, k, key));
            }
        }
    }
    Some(Block {
        head,
        end,
        indent,
        entries,
    })
}

fn split(src: &str) -> (Vec<String>, bool) {
    let crlf = src.contains("\r\n");
    let lf = src.replace("\r\n", "\n");
    let body = lf.strip_suffix('\n').unwrap_or(&lf);
    (body.split('\n').map(str::to_string).collect(), crlf)
}

fn join(lines: &[String], crlf: bool) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    if crlf {
        s = s.replace('\n', "\r\n");
    }
    s
}

fn entry_lines(key: &str, spec: &Spec, indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    match spec {
        Spec::Version(v) => vec![format!("{pad}{}: {}", yaml::string(key), yaml::string(v))],
        Spec::FromMonorepo => vec![
            format!("{pad}{}:", yaml::string(key)),
            format!("{pad}  from: monorepo"),
        ],
    }
}

/// Add `key` (or replace its current entry), keeping the table in key order when it already is.
pub fn add_dep(src: &str, key: &str, spec: &Spec) -> Result<String, String> {
    let (mut lines, crlf) = split(src);
    let Some(b) = find_block(&lines) else {
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.push(String::new());
        lines.push("dependencies:".into());
        lines.extend(entry_lines(key, spec, 2));
        return Ok(join(&lines, crlf));
    };
    let mut at = b.end;
    let mut remove: Option<(usize, usize)> = None;
    let sorted = b.entries.windows(2).all(|w| w[0].2 <= w[1].2);
    for (n, (first, _, k)) in b.entries.iter().enumerate() {
        if k == key {
            let stop = b.entries.get(n + 1).map_or(b.end, |e| e.0);
            remove = Some((*first, stop));
            at = *first;
            break;
        }
        if sorted && at == b.end && k.as_str() > key {
            at = *first;
        }
    }
    let new = entry_lines(key, spec, b.indent);
    // A table written as `dependencies: {}` becomes a block.
    let head_is_flow =
        lines[b.head].split(" #").next().unwrap_or("").trim_end() == "dependencies: {}";
    if let Some((from, to)) = remove {
        lines.splice(from..to, new);
    } else {
        lines.splice(at..at, new);
    }
    if head_is_flow {
        lines[b.head] = "dependencies:".into();
    }
    Ok(join(&lines, crlf))
}

/// Remove `key`; the whole `dependencies:` section goes when it would be left empty.
pub fn remove_dep(src: &str, key: &str) -> Result<String, String> {
    let (mut lines, crlf) = split(src);
    let b = find_block(&lines).ok_or("there is no `dependencies:` section")?;
    let n = b
        .entries
        .iter()
        .position(|e| e.2 == key)
        .ok_or_else(|| format!("`{key}` is not in `dependencies:`"))?;
    let stop = b.entries.get(n + 1).map_or(b.end, |e| e.0);
    let (from, to) = (b.entries[n].0, stop);
    if b.entries.len() == 1 {
        let mut from_head = b.head;
        // Take the blank line that separated the section from the one above.
        if from_head > 0 && lines[from_head - 1].trim().is_empty() {
            from_head -= 1;
        }
        lines.drain(from_head..b.end);
    } else {
        lines.drain(from..to);
    }
    Ok(join(&lines, crlf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_manifest;

    const BASE: &str = "# my package\nformat: 1\n\npackage:\n  name: demo\n  version: 0.1.0\n\ndependencies:\n  # parsing\n  json: 1.2.0\n  units:\n    version: 0.2.4\n    target: avr\n  wire: 0.7.2\n\ngenerate: {}\n";

    fn ok(s: &str) {
        parse_manifest("t", s).unwrap_or_else(|e| panic!("{e}\n{s}"));
    }

    #[test]
    fn add_keeps_order_and_comments() {
        let s = add_dep(BASE, "clock", &Spec::Version("1.1.0".into())).unwrap();
        ok(&s);
        // sorted table: `clock` goes before `json` and before the comment that belongs to json.
        assert!(
            s.contains("dependencies:\n  clock: 1.1.0\n  # parsing\n  json: 1.2.0\n"),
            "{s}"
        );
        let s = add_dep(BASE, "zed", &Spec::FromMonorepo).unwrap();
        ok(&s);
        assert!(
            s.contains("  wire: 0.7.2\n  zed:\n    from: monorepo\n\ngenerate"),
            "{s}"
        );
    }

    #[test]
    fn add_replaces_an_existing_entry_with_all_its_lines() {
        let s = add_dep(BASE, "units", &Spec::Version("0.3.0".into())).unwrap();
        ok(&s);
        assert!(s.contains("  units: 0.3.0\n  wire"), "{s}");
        assert!(!s.contains("target: avr"), "{s}");
    }

    #[test]
    fn add_creates_the_section_when_missing_or_flow() {
        let s = add_dep(
            "format: 1\npackage:\n  name: a\n  version: 1.0.0\n",
            "json",
            &Spec::Version("1.5.0".into()),
        )
        .unwrap();
        ok(&s);
        assert!(s.ends_with("\ndependencies:\n  json: 1.5.0\n"), "{s}");
        let s = add_dep(
            "format: 1\npackage:\n  name: a\n  version: 1.0.0\ndependencies: {}\n",
            "json/v2",
            &Spec::Version("2.0.1".into()),
        )
        .unwrap();
        ok(&s);
        assert!(s.contains("dependencies:\n  json/v2: 2.0.1\n"), "{s}");
    }

    #[test]
    fn remove_takes_the_entry_and_its_comment() {
        let s = remove_dep(BASE, "json").unwrap();
        ok(&s);
        assert!(!s.contains("json") && !s.contains("# parsing"), "{s}");
        let s = remove_dep(BASE, "units").unwrap();
        ok(&s);
        assert!(
            !s.contains("units") && !s.contains("target") && s.contains("wire"),
            "{s}"
        );
        assert!(remove_dep(BASE, "nope").is_err());
    }

    #[test]
    fn removing_the_last_one_removes_the_section() {
        let one =
            "format: 1\npackage:\n  name: a\n  version: 1.0.0\n\ndependencies:\n  json: 1.5.0\n";
        let s = remove_dep(one, "json").unwrap();
        ok(&s);
        assert_eq!(s, "format: 1\npackage:\n  name: a\n  version: 1.0.0\n");
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let crlf = BASE.replace('\n', "\r\n");
        let s = add_dep(&crlf, "clock", &Spec::Version("1.1.0".into())).unwrap();
        assert!(s.contains("\r\n") && !s.replace("\r\n", "").contains('\n'));
        ok(&s);
    }
}
