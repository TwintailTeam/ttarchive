#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        Self::new_in(std::env::temp_dir(), tag)
    }

    pub fn new_in(base: impl AsRef<Path>, tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!("ttarchive-{tag}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        let path = base.as_ref().join(unique);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.path.join(rel)
    }

    pub fn write(&self, rel: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> PathBuf {
        let target = self.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&target, contents).expect("write file");
        target
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn pseudo_random(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 16) as u8
        })
        .collect()
}

pub fn compressible(len: usize) -> Vec<u8> {
    let phrase = b"the quick brown fox jumps over the lazy dog ";
    phrase.iter().copied().cycle().take(len).collect()
}

pub fn filled(len: usize, byte: u8) -> Vec<u8> {
    vec![byte; len]
}

pub fn two_symbol(len: usize, seed: u32) -> Vec<u8> {
    pseudo_random(len, seed).into_iter().map(|b| if b & 1 == 0 { b'a' } else { b'b' }).collect()
}

pub fn call_heavy(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 5);
    let mut target = 0x1000u32;
    while out.len() < len {
        out.push(if out.len() % 2 == 0 { 0xE8 } else { 0xE9 });
        out.extend_from_slice(&target.to_le_bytes());
        target = target.wrapping_add(0x40);
    }
    out.truncate(len);
    out
}

pub struct Shape {
    pub name: &'static str,
    pub files: Vec<(String, Vec<u8>)>,
}

pub fn shapes() -> Vec<Shape> {
    let mut many = Vec::new();
    for index in 0..1_500u32 {
        let len = (index as usize * 37) % 300;
        let body = match index % 3 {
            0 => compressible(len),
            1 => pseudo_random(len, index),
            _ => filled(len, index as u8),
        };
        many.push((format!("d{}/f{index}.bin", index % 40), body));
    }

    let mut edges = Vec::new();
    for len in [0usize, 1, 2, 31, 32, 4_095, 4_096, 32_767, 32_768, 32_769, 65_535, 65_536, 65_537, 1_048_575, 1_048_576, 1_048_577] {
        edges.push((format!("compressible-{len}.bin"), compressible(len)));
        edges.push((format!("noise-{len}.bin"), pseudo_random(len, len as u32)));
    }

    vec![
        Shape {
            name: "stored-chunks",
            files: vec![
                ("a-prose.txt".into(), compressible(1_500_000)),
                ("b-noise.bin".into(), pseudo_random(5_000_000, 41)),
                ("c-prose.txt".into(), compressible(2_000_000)),
                ("d-noise.bin".into(), pseudo_random(700_000, 42)),
                ("e-prose.txt".into(), compressible(300_000)),
            ],
        },
        Shape { name: "many-small", files: many },
        Shape { name: "edge-sizes", files: edges },
        Shape {
            name: "pathological",
            files: vec![
                ("zeros.bin".into(), filled(3_000_000, 0x00)),
                ("ones.bin".into(), filled(1_000_000, 0xFF)),
                ("two-symbol.txt".into(), two_symbol(2_000_000, 7)),
                ("one-byte.bin".into(), vec![0x5A]),
                ("empty.bin".into(), Vec::new()),
            ],
        },
        Shape {
            name: "filter-bait",
            files: vec![("calls.bin".into(), call_heavy(600_000)), ("calls-then-noise.bin".into(), [call_heavy(100_000), pseudo_random(400_000, 9)].concat())],
        },
        Shape {
            name: "already-compressed",
            files: vec![
                (
                    "noise.gz".into(),
                    ttarchive::codecs::gzip::compress(&pseudo_random(1_500_000, 5), ttarchive::Level::Default, &ttarchive::codecs::gzip::Member::default())
                        .expect("gzip"),
                ),
                ("prose.txt".into(), compressible(500_000)),
            ],
        },
    ]
}

pub fn write_shape(dir: &TempDir, root: &str, shape: &Shape) -> PathBuf {
    for (name, body) in &shape.files {
        dir.write(format!("{root}/{name}"), body);
    }
    dir.join(root)
}

pub fn snapshot(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path.strip_prefix(root).expect("under root").to_string_lossy().replace('\\', "/");
                out.insert(name, std::fs::read(&path).expect("read"));
            }
        }
    }
    out
}

fn runs(program: &str) -> bool {
    let status = Command::new(program).arg("--version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    match program {
        "jar" | "java" | "python3" => status.is_ok_and(|s| s.success()),
        _ => status.is_ok(),
    }
}

pub fn resolve(tool: &str) -> String {
    let candidates: &[&str] = match tool {
        "7z" => &["7z", "7zz"],
        _ => return tool.to_string(),
    };
    candidates.iter().find(|candidate| runs(candidate)).map_or_else(|| tool.to_string(), |found| found.to_string())
}

pub fn have(tool: &str) -> bool {
    runs(&resolve(tool))
}

pub fn gnu_tar() -> Option<String> {
    ["tar", "gtar"]
        .into_iter()
        .find(|program| {
            Command::new(program).arg("--version").stdin(Stdio::null()).output().is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("GNU tar"))
        })
        .map(String::from)
}

pub fn skip(tool: &str) {
    eprintln!("skipping: {tool} is not installed");
}

pub fn machine_code() -> Vec<u8> {
    let mut out = std::env::current_exe().ok().and_then(|path| std::fs::read(path).ok()).unwrap_or_default();
    out.truncate(4 << 20);
    if out.len() < 200_000 {
        out.extend_from_slice(&call_heavy(200_000));
    }
    out
}
