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

pub fn stage(
    paths: &[PathBuf],
    progress: &std::sync::atomic::AtomicU64,
    total: &std::sync::atomic::AtomicU64,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<(Vec<Entry>, tempfile::TempDir), &'static str> {
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering::Relaxed;

    let mut entries = snapshot(paths)?;
    total.store(entries.iter().map(|entry| entry.size).sum(), Relaxed);
    let directory = tempfile::tempdir().map_err(|_| "Не удалось создать временную папку.")?;
    let mut buffer = vec![0; 256 * 1024];
    for (index, entry) in entries.iter_mut().enumerate() {
        if cancelled.load(Relaxed) {
            return Err("Копирование отменено.");
        }
        if entry.directory {
            continue;
        }
        let target = directory.path().join(index.to_string());
        let mut input = fs::File::open(&entry.source).map_err(|_| "Исходный файл недоступен.")?;
        let mut output =
            fs::File::create(&target).map_err(|_| "Не удалось создать временный файл.")?;
        let mut remaining = entry.size;
        while remaining > 0 {
            if cancelled.load(Relaxed) {
                return Err("Копирование отменено.");
            }
            let count = remaining.min(buffer.len() as u64) as usize;
            input
                .read_exact(&mut buffer[..count])
                .map_err(|_| "Не удалось прочитать файл из исходного сеанса.")?;
            output
                .write_all(&buffer[..count])
                .map_err(|_| "Не удалось записать временный файл. Проверьте свободное место.")?;
            remaining -= count as u64;
            progress.fetch_add(count as u64, Relaxed);
        }
        entry.source = target;
    }
    Ok((entries, directory))
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
    fn staged_files_survive_source_removal_and_report_bytes() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("file.bin");
        let payload = vec![17; 600_000];
        fs::write(&file, &payload).unwrap();
        let done = AtomicU64::new(0);
        let total = AtomicU64::new(0);
        let cancelled = AtomicBool::new(false);
        let (entries, staging) = stage(&[file], &done, &total, &cancelled).unwrap();
        drop(source);
        assert_eq!(done.load(Ordering::Relaxed), payload.len() as u64);
        assert_eq!(total.load(Ordering::Relaxed), payload.len() as u64);
        assert_eq!(fs::read(&entries[0].source).unwrap(), payload);
        drop(staging);
        assert!(!entries[0].source.exists());
    }

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
