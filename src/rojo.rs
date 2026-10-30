use crate::cli::ColorChoice;
use crate::sourcemap;
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How this run invokes the `rojo` binary. `cwd` is the original project directory, so a rokit
/// shim or mise finds its manifest.
#[derive(Clone, Debug)]
pub struct Rojo {
    pub cwd: PathBuf,
    pub color: ColorChoice,
    pub verbose: u8,
}

impl Rojo {
    /// `rojo sourcemap --absolute <project>`. On a non-zero exit the error's first line is
    /// v1.2.4's message with Rojo's exit code, and the lines after it are Rojo's stderr.
    pub fn sourcemap(&self, project: &Path) -> anyhow::Result<sourcemap::Node> {
        let out = Command::new("rojo")
            .arg("sourcemap")
            .arg("--absolute")
            .arg(project)
            .current_dir(&self.cwd)
            .output()
            .with_context(|| format!("cannot run rojo in {}", self.cwd.display()))?;

        if !out.status.success() {
            let code = out
                .status
                .code()
                .map_or_else(|| "none".to_string(), |c| c.to_string());
            let stderr = String::from_utf8_lossy(&out.stderr);

            anyhow::bail!(
                "Rojo sourcemap generation failed for {} with exit code {code}\n{}",
                project.display(),
                stderr.trim_end()
            );
        }

        let json = String::from_utf8_lossy(&out.stdout);

        sourcemap::parse(&json)
            .with_context(|| format!("Rojo's sourcemap for {} did not parse", project.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_sourcemap_carries_the_exit_code_and_stderr() {
        let rojo = Rojo {
            cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            color: ColorChoice::Never,
            verbose: 0,
        };
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("default.project.json");
        std::fs::write(&project, r#"{"name": "X", "tree": {"$path": "missing"}}"#).unwrap();

        let err = rojo.sourcemap(&project).unwrap_err().to_string();
        let mut lines = err.lines();
        assert_eq!(
            lines.next(),
            Some(
                format!(
                    "Rojo sourcemap generation failed for {} with exit code 1",
                    project.display()
                )
                .as_str()
            )
        );
        assert!(lines.next().is_some(), "{err}");
    }
}
