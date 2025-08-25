use crate::error::TaskError;

/// Extract YAML front-matter from a markdown file.
/// Accepts documents starting with:
/// ---\n<yaml>\n---\n<body...>
pub fn extract_yaml_front_matter(input: &str) -> Result<&str, TaskError> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("---") {
        return Err(TaskError::InvalidTaskFile(
            "missing YAML front-matter '---' header".to_string(),
        ));
    }

    // Find end of the first line (the opening '---')
    let first_nl = match trimmed.find('\n') {
        Some(i) => i,
        None => {
            return Err(TaskError::InvalidTaskFile(
                "malformed YAML front-matter".to_string(),
            ))
        }
    };
    let first_line = trimmed[..first_nl].trim_end_matches('\r');
    if first_line != "---" {
        return Err(TaskError::InvalidTaskFile(
            "malformed YAML front-matter".to_string(),
        ));
    }

    let yaml_start = first_nl + 1; // after the first newline
    let mut pos = yaml_start;
    let mut found_end = None;
    for line in trimmed[yaml_start..].split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content == "---" {
            found_end = Some(pos);
            break;
        }
        pos += line.len();
    }

    match found_end {
        Some(yaml_end) => Ok(&trimmed[yaml_start..yaml_end]),
        None => Err(TaskError::InvalidTaskFile(
            "missing closing '---' for YAML front-matter".to_string(),
        )),
    }
}
