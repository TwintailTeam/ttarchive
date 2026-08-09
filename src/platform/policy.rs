use std::path::{Component, Path, PathBuf};

use crate::utils::error::PathRejection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NamePolicy {
    Strict,

    #[default]
    Native,
}

const WINDOWS_RESERVED: [&str; 24] = [
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5",
    "LPT6", "LPT7", "LPT8", "LPT9",
];

const WINDOWS_ILLEGAL: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];

const ON_WINDOWS: bool = cfg!(windows);

pub fn components(name: &str) -> impl Iterator<Item = &str> {
    name.split(['/', '\\']).filter(|c| !c.is_empty() && *c != ".")
}

fn is_rooted(name: &str) -> bool {
    matches!(name.as_bytes().first(), Some(b'/') | Some(b'\\'))
}

fn has_drive_prefix(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
}

fn is_absolute_anywhere(name: &str) -> bool {
    is_rooted(name) || has_drive_prefix(name)
}

fn check_windows_component(component: &str) -> Result<(), PathRejection> {
    if component.contains(WINDOWS_ILLEGAL) {
        return Err(PathRejection::IllegalCharacter);
    }

    if component.ends_with('.') || component.ends_with(' ') {
        return Err(PathRejection::IllegalCharacter);
    }

    let stem = component.split('.').next().unwrap_or(component);
    if WINDOWS_RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        return Err(PathRejection::IllegalCharacter);
    }

    Ok(())
}

pub fn validate(name: &str, policy: NamePolicy) -> Result<(), PathRejection> {
    if name.is_empty() {
        return Err(PathRejection::Empty);
    }

    if name.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
        return Err(PathRejection::IllegalCharacter);
    }

    if is_rooted(name) {
        return Err(PathRejection::Absolute);
    }

    if (policy == NamePolicy::Strict || ON_WINDOWS) && has_drive_prefix(name) {
        return Err(PathRejection::Absolute);
    }

    let mut any = false;
    for component in components(name) {
        if component == ".." {
            return Err(PathRejection::ParentTraversal);
        }
        any = true;

        if policy == NamePolicy::Strict || ON_WINDOWS {
            check_windows_component(component)?;
        }
    }

    if !any {
        return Err(PathRejection::Empty);
    }

    Ok(())
}

pub fn to_relative_path(name: &str, policy: NamePolicy) -> Result<PathBuf, PathRejection> {
    validate(name, policy)?;
    Ok(components(name).collect())
}

pub fn has_directory_suffix(name: &str) -> bool {
    name.ends_with('/') || name.ends_with('\\')
}

pub fn to_entry_name(path: &Path, is_dir: bool) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => return None,
        }
    }

    if parts.is_empty() {
        return None;
    }

    let mut name = parts.join("/");
    if is_dir {
        name.push('/');
    }
    Some(name)
}

pub fn symlink_target_escapes(root: &Path, link_path: &Path, target: &str) -> bool {
    if is_absolute_anywhere(target) {
        return true;
    }

    let Some(parent) = link_path.parent() else {
        return true;
    };

    let Ok(rel) = parent.strip_prefix(root) else {
        return true;
    };
    let mut depth = rel.components().filter(|c| matches!(c, Component::Normal(_))).count() as i64;

    for component in target.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }

    false
}
