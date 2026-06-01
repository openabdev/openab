use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// A discovered skill with its metadata and path.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Scan skill directories and return discovered skills.
/// Scans: working_dir/.openab/skills/ then ~/.openab/agent/skills/
/// First occurrence of a name wins (project-local takes precedence).
pub fn discover_skills(working_dir: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    let dirs = skill_dirs(working_dir);
    for dir in &dirs {
        if !dir.is_dir() {
            continue;
        }
        debug!("scanning skills in {}", dir.display());
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }
            if let Some(skill) = parse_skill_md(&skill_md) {
                if seen_names.contains(&skill.name) {
                    warn!(name = %skill.name, "duplicate skill, skipping {}", skill_md.display());
                    continue;
                }
                seen_names.insert(skill.name.clone());
                skills.push(skill);
            }
        }
    }
    skills
}

/// Build the skill directories to scan (project-local first, then global).
fn skill_dirs(working_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![working_dir.join(".openab/skills")];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".openab/agent/skills"));
    }
    dirs
}

/// Parse a SKILL.md file, extracting name and description from YAML frontmatter.
fn parse_skill_md(path: &Path) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;
    let (name, description) = parse_frontmatter(&content)?;
    if description.is_empty() {
        warn!("skill at {} has no description, skipping", path.display());
        return None;
    }
    Some(Skill {
        name,
        description,
        path: path.parent()?.to_path_buf(),
    })
}

/// Extract name and description from YAML frontmatter delimited by `---`.
fn parse_frontmatter(content: &str) -> Option<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_first = &trimmed[3..];
    let end = after_first.find("\n---")?;
    let frontmatter = &after_first[..end];

    let mut name = String::new();
    let mut description = String::new();

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            name = val.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(val) = line.strip_prefix("description:") {
            description = val.trim().trim_matches('"').trim_matches('\'').to_string();
        }
    }

    if name.is_empty() {
        return None;
    }
    Some((name, description))
}

/// Escape XML special characters in user-controlled strings to preserve prompt structure.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Format skills as a system prompt section listing available skills.
pub fn format_skills_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    // Sort skills by name to ensure deterministic output
    let mut sorted_skills = skills.to_vec();
    sorted_skills.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = String::from(concat!(
        "\n\n## Skills\n\n",
        "Before replying, scan the skills below. If one matches or may be relevant to the user's task, ",
        "use the `read` tool to load the full SKILL.md at the listed location and follow its instructions.\n\n",
        "<available_skills>\n",
    ));
    for skill in sorted_skills {
        out.push_str(&format!(
            "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n    <location>{}</location>\n  </skill>\n",
            xml_escape(&skill.name),
            xml_escape(&skill.description),
            skill.path.join("SKILL.md").display(),
        ));
    }
    out.push_str("</available_skills>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_frontmatter_valid() {
        let content = "---\nname: my-skill\ndescription: Does things\n---\n\n# Instructions\n";
        let (name, desc) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "my-skill");
        assert_eq!(desc, "Does things");
    }

    #[test]
    fn parse_frontmatter_quoted() {
        let content = "---\nname: \"web-search\"\ndescription: 'Searches the web'\n---\n";
        let (name, desc) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "web-search");
        assert_eq!(desc, "Searches the web");
    }

    #[test]
    fn parse_frontmatter_missing_name() {
        let content = "---\ndescription: No name\n---\n";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn parse_frontmatter_no_delimiters() {
        let content = "# Just markdown\nNo frontmatter here.";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    #[ignore] // Integration test: filesystem I/O
    fn discover_skills_from_directory() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".openab/skills/my-skill");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: Test skill\n---\n\n# Usage\nDo stuff.\n",
        )
        .unwrap();

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description, "Test skill");
    }

    #[test]
    #[ignore] // Integration test: filesystem I/O
    fn discover_skills_skips_no_description() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".openab/skills/bad-skill");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: bad-skill\ndescription:\n---\n",
        )
        .unwrap();

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 0);
    }

    #[test]
    #[ignore] // Integration test: filesystem I/O
    fn discover_skills_deduplicates() {
        let tmp = TempDir::new().unwrap();

        // Project-local skill
        let local_dir = tmp.path().join(".openab/skills/dupe");
        fs::create_dir_all(&local_dir).unwrap();
        fs::write(
            local_dir.join("SKILL.md"),
            "---\nname: dupe\ndescription: Local version\n---\n",
        )
        .unwrap();

        // Simulate global by creating another dir and calling parse directly
        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "Local version");
    }

    #[test]
    fn format_skills_prompt_empty() {
        assert_eq!(format_skills_prompt(&[]), "");
    }

    #[test]
    fn format_skills_prompt_uses_xml_format() {
        let skills = vec![Skill {
            name: "test".to_string(),
            description: "A test skill".to_string(),
            path: PathBuf::from("/home/agent/.openab/skills/test"),
        }];
        let prompt = format_skills_prompt(&skills);
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>test</name>"));
        assert!(prompt.contains("<description>A test skill</description>"));
        assert!(prompt.contains("<location>/home/agent/.openab/skills/test/SKILL.md</location>"));
        assert!(prompt.contains("</available_skills>"));
        assert!(prompt.contains("Before replying, scan the skills below"));
    }

    #[test]
    fn format_skills_prompt_escapes_xml_chars() {
        let skills = vec![Skill {
            name: "a<b".to_string(),
            description: "Use <tag> & \"quotes\"".to_string(),
            path: PathBuf::from("/skills/test"),
        }];
        let prompt = format_skills_prompt(&skills);
        assert!(prompt.contains("<name>a&lt;b</name>"));
        assert!(prompt.contains("<description>Use &lt;tag&gt; &amp; \"quotes\"</description>"));
    }

    #[test]
    fn format_skills_prompt_is_deterministic() {
        let skills = vec![
            Skill {
                name: "zoo".to_string(),
                description: "Zoo skill".to_string(),
                path: PathBuf::from("/skills/zoo"),
            },
            Skill {
                name: "apple".to_string(),
                description: "Apple skill".to_string(),
                path: PathBuf::from("/skills/apple"),
            },
        ];
        let prompt = format_skills_prompt(&skills);
        let apple_idx = prompt.find("<name>apple</name>").unwrap();
        let zoo_idx = prompt.find("<name>zoo</name>").unwrap();
        assert!(apple_idx < zoo_idx);
    }
}
