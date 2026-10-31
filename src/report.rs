use crate::dealias::Replacement;
use crate::log;
use crate::scopes::{Scope, ScopeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One scope's rows under its title (the original project path, or `<.luaurc> (orphan)`).
pub struct ProjectLog {
    pub title: String,
    pub rows: Vec<Replacement>,
}

/// One log per scope of `set` that made rows, titled with its original path (`original` maps a
/// path in the tree back to the project). A scope that made none prints v1.2.4's `-v` line with
/// how many files it checked instead. `rows` and `checked` are indexed like `set.scopes`.
pub fn logs(
    set: &ScopeSet,
    rows: Vec<Vec<Replacement>>,
    checked: &[usize],
    original: &dyn Fn(&Path) -> PathBuf,
) -> Vec<ProjectLog> {
    let mut logs = Vec::new();

    for ((scope, rows), checked) in set.scopes.iter().zip(rows).zip(checked) {
        if rows.is_empty() {
            if log::verbose() > 0 {
                match &scope.project {
                    Some(project) => log::info(&format!(
                        "Processed {} - checked {checked} files, no alias requires found",
                        project.display()
                    )),
                    None => log::info(&format!(
                        "Processed {} (orphan mode) - checked {checked} files, no alias requires found",
                        scope.luaurc.display()
                    )),
                }
            }

            continue;
        }

        logs.push(ProjectLog {
            title: title(scope, original),
            rows,
        });
    }

    logs
}

/// A scope's title: its original project path, or `<.luaurc> (orphan)`.
pub fn title(scope: &Scope, original: &dyn Fn(&Path) -> PathBuf) -> String {
    match &scope.project {
        Some(project) => original(project).display().to_string(),
        None => format!("{} (orphan)", original(&scope.luaurc).display()),
    }
}

/// v1.2.4's startup table (`init.luau:301-339`): one `Processed project:` line per log with rows,
/// every row padded to widths shared by all logs, then the time taken. Printed only with `-v`
/// (owner ruling 2026-10-31; v1.2.4 always printed it).
pub fn print(logs: &[ProjectLog], elapsed: Duration) {
    if log::verbose() == 0 {
        return;
    }

    let logs: Vec<&ProjectLog> = logs.iter().filter(|l| !l.rows.is_empty()).collect();

    if logs.is_empty() {
        return;
    }

    let rows = logs.iter().flat_map(|l| &l.rows);
    let left = rows.clone().map(|r| r.label.len()).max().unwrap_or(0);
    let right = rows.map(|r| r.from.len()).max().unwrap_or(0);

    for entry in logs {
        // v1.2.4 sorted by the original require's length alone, which left ties in table order
        let mut rows: Vec<(usize, &Replacement)> = entry.rows.iter().enumerate().collect();
        rows.sort_by(|(ai, a), (bi, b)| {
            a.from
                .len()
                .cmp(&b.from.len())
                .then_with(|| a.label.cmp(&b.label))
                .then(ai.cmp(bi))
        });

        log::success(&format!("Processed project: {}", entry.title));

        // padded by byte length, as v1.2.4's string.len
        for (_, row) in rows {
            log::info(&format!(
                "{}{} {}{} -> {}",
                row.label,
                " ".repeat(left - row.label.len()),
                row.from,
                " ".repeat(right - row.from.len()),
                row.to
            ));
        }
    }

    log::info(&format!("Done in {}ms\n", millis(elapsed)));
}

/// Milliseconds rounded to two decimals, without trailing zeros, as Lua prints the number.
fn millis(elapsed: Duration) -> String {
    let text = format!("{:.2}", elapsed.as_secs_f64() * 1000.0);
    let text = text.trim_end_matches('0').trim_end_matches('.');

    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_formats_like_lua() {
        assert_eq!(millis(Duration::from_micros(7270)), "7.27");
        assert_eq!(millis(Duration::from_millis(12)), "12");
        assert_eq!(millis(Duration::from_micros(12500)), "12.5");
        assert_eq!(millis(Duration::ZERO), "0");
    }
}
