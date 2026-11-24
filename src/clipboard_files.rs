//! A bounded snapshot of local files offered to the RDP clipboard.
use std::{
    fs,
    path::{Path, PathBuf},
};

const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Debug)]
pub struct Entry {
    pub source: PathBuf,
    pub remote_name: String,
    pub size: u64,
    pub directory: bool,
}

pub fn snapshot(paths: &[PathBuf]) -> Result<Vec<Entry>, &'static str> {
    if paths.is_empty() || paths.len() > MAX_ENTRIES {
        return Err("Выберите не более 4096 файлов и папок.");
    }
    let mut entries = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Имя файла должно быть текстом UTF-8.")?;
        visit(path, name.to_owned(), 0, &mut entries)?;
    }
    Ok(entries)
}

fn visit(
    path: &Path,
    remote_name: String,
    depth: usize,
    entries: &mut Vec<Entry>,
) -> Result<(), &'static str> {
    if entries.len() >= MAX_ENTRIES || depth > 64 || remote_name.encode_utf16().count() > 259 {
        return Err("Слишком много файлов или слишком длинное имя для RDP.");
    }
    let component = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Имя файла должно быть текстом UTF-8.")?;
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if component.is_empty()
        || reserved
        || component == "."
        || component == ".."
        || component.ends_with([' ', '.'])
        || component
            .chars()
            .any(|ch| ch < ' ' || "<>:\"/\\|?*".contains(ch))
    {
        return Err("Имя файла содержит символы, недопустимые в Windows.");
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| "Не удалось прочитать файл.")?;
    if metadata.file_type().is_symlink() {
        return Err("Символические ссылки нельзя копировать через буфер RDP.");
    }
    let directory = metadata.is_dir();
    if !directory && !metadata.is_file() {
        return Err("Через буфер RDP можно копировать только обычные файлы и папки.");
    }
    if !directory && metadata.len() >= 2_147_483_648 {
        return Err("Файл больше 2 ГБ пока нельзя копировать через буфер RDP.");
    }
    entries.push(Entry {
        source: path.to_path_buf(),
        remote_name: remote_name.clone(),
        size: if directory { 0 } else { metadata.len() },
        directory,
    });
    if directory {
        let mut children = fs::read_dir(path)
            .map_err(|_| "Не удалось открыть папку.")?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Не удалось прочитать папку.")?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| "Имя файла должно быть текстом UTF-8.")?;
            visit(
                &child.path(),
                format!("{remote_name}\\{name}"),
                depth + 1,
                entries,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collects_nested_folders_and_rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("file.txt"), b"abc").unwrap();
        let items = snapshot(&[folder]).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].remote_name, "folder\\file.txt");
        assert_eq!(items[1].size, 3);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&items[1].source, dir.path().join("link")).unwrap();
            assert!(snapshot(&[dir.path().join("link")]).is_err());
        }
    }
}
