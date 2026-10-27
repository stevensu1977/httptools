use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

enum Format {
    Json,
    Csv,
}

pub struct ReportFile {
    path: PathBuf,
    file: BufWriter<File>,
    format: Format,
}

impl ReportFile {
    /// Check destinations before sending load, and preserve earlier reports.
    pub fn create(path: &Path) -> Result<Self> {
        let format = match path.extension().and_then(|s| s.to_str()) {
            Some(extension) if extension.eq_ignore_ascii_case("json") => Format::Json,
            Some(extension) if extension.eq_ignore_ascii_case("csv") => Format::Csv,
            _ => bail!("report path must end in .json or .csv: {}", path.display()),
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating report directory {}", parent.display()))?;
        }
        let file = File::create_new(path).with_context(|| {
            format!(
                "creating report {}; use a new filename if it already exists",
                path.display()
            )
        })?;
        Ok(Self {
            path: path.to_owned(),
            file: BufWriter::new(file),
            format,
        })
    }

    pub fn save(&mut self, summary: &Value) -> Result<()> {
        self.write(summary)
            .with_context(|| format!("writing report {}", self.path.display()))?;
        eprintln!("Report saved: {}", self.path.display());
        Ok(())
    }

    fn write(&mut self, summary: &Value) -> Result<()> {
        match self.format {
            Format::Json => {
                serde_json::to_writer_pretty(&mut self.file, summary)?;
                writeln!(self.file)?;
            }
            Format::Csv => {
                // One row per run; flatten latency into columns such as latency_p99_ms.
                let mut columns = Vec::new();
                for (key, value) in summary.as_object().context("report must be an object")? {
                    if key == "latency_ms" {
                        for (percentile, latency) in value.as_object().context("missing latency")? {
                            columns.push((format!("latency_{percentile}_ms"), latency.to_string()));
                        }
                    } else {
                        let cell = match value {
                            Value::String(s) => s.clone(),
                            Value::Null => String::new(),
                            _ => value.to_string(),
                        };
                        columns.push((key.clone(), cell));
                    }
                }
                let mut writer = csv::Writer::from_writer(&mut self.file);
                writer.write_record(columns.iter().map(|(key, _)| key))?;
                writer.write_record(columns.iter().map(|(_, value)| value))?;
                writer.flush()?;
            }
        }
        self.file.flush()?;
        Ok(())
    }
}
