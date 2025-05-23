use std::cell::LazyCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use regex::{Regex, RegexBuilder};

use crate::{Config, ConfigParseError};

pub const SOURCE_FILE_RE: LazyCell<Regex> = LazyCell::new(|| {
    RegexBuilder::new(r#"^\s*source\s+"(?<source_file>.+\.kdl)"\s*?$"#)
        .multi_line(true)
        .case_insensitive(true)
        .build()
        .unwrap()
});

pub fn expand_source_file(
    file_path: &Path,
    sourced_paths: &mut HashMap<PathBuf, bool>,
) -> Result<String, ConfigParseError> {
    let file_content = std::fs::read_to_string(file_path).map_err(ConfigParseError::IoError)?;
    let base_path = file_path.parent().unwrap();
    let mut last_match_pos = 0;
    let mut expanded_file_content = String::new();

    for caps in SOURCE_FILE_RE.captures_iter(file_content.as_str()) {
        let Some(source_file) = caps.name("source_file") else {
            unreachable!("source_file must always be available")
        };

        expanded_file_content.push_str(&file_content[last_match_pos..source_file.start()]);

        let source_file_str = source_file.as_str();
        let user_source_path = Path::new(source_file_str);
        let absolute_source_path = base_path.join(source_file_str);
        let final_source_path = if user_source_path.is_absolute() {
            user_source_path
        } else {
            absolute_source_path.as_path()
        };

        if sourced_paths
            .insert(final_source_path.to_path_buf(), true)
            .is_some_and(|v| v)
        {
            return Err(ConfigParseError::CircularSourceError(
                file_path.to_path_buf(),
                final_source_path.to_path_buf(),
            ));
        }

        let sourced_content = expand_source_file(final_source_path, sourced_paths)?;

        sourced_paths.insert(final_source_path.to_path_buf(), false);

        Config::parse(file_path.to_str().unwrap(), sourced_content.as_str())?;

        expanded_file_content.push_str(&sourced_content);
        last_match_pos = source_file.end();
    }

    expanded_file_content.push_str(&file_content[last_match_pos..]);
    Ok(expanded_file_content)
}

/// `Regex` that implements `PartialEq` by its string form.
#[derive(Debug, Clone)]
pub struct RegexEq(pub Regex);

impl PartialEq for RegexEq {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for RegexEq {}

impl FromStr for RegexEq {
    type Err = <Regex as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Regex::from_str(s).map(Self)
    }
}
