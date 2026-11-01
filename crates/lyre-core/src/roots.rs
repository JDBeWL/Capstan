//! 曲库根，用户交代过的音乐文件夹。
//! 曲库本身只存曲目，没有「这些曲目是从哪个文件夹扫来的」这层信息；
//! 补扫新增曲目、记录已加入的文件夹，均需单独维护一份根。
//! 只记目录。单个音频文件随导入进库，但不会成为「以后还要再看一眼」的地方。

use std::path::{Component, Path, PathBuf};

/// 一组曲库根，顺序是用户加进来的顺序。
/// 不变量。任意两项都不互相包含（父目录已经在册时，子目录不该再单独占一行）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roots {
    items: Vec<PathBuf>,
}

impl Roots {
    pub fn new() -> Self {
        Self::default()
    }

    /// 收一组路径。逐条走 [`Self::add`]，顺序保留。
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut roots = Self::new();
        for path in paths {
            roots.add(path);
        }
        roots
    }

    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 收一个目录。已经被在册的根覆盖就返回 `false`；比某个在册的根更外层，
    /// 就把里面那些并掉。同一个目录不该在两行里各算一次，也不该被扫两遍。
    pub fn add(&mut self, path: impl Into<PathBuf>) -> bool {
        let path = normalise(&path.into());
        if self.items.iter().any(|known| under(&path, known)) {
            return false;
        }
        self.items.retain(|known| !under(known, &path));
        self.items.push(path);
        true
    }

    /// 移掉一条。只认完全相同的目录，不做包含判断，移除父目录不该顺手带走子目录那一行。
    pub fn remove(&mut self, path: &Path) -> bool {
        let path = normalise(path);
        let before = self.items.len();
        self.items.retain(|known| *known != path);
        self.items.len() != before
    }

    /// 这个路径落在哪个根下面。用来按根统计曲目数、按根剔除。
    /// 传进来的可能是相对写法（命令行参数、拖放），先归一再比。
    pub fn root_of(&self, path: &Path) -> Option<&Path> {
        let path = normalise(path);
        self.items
            .iter()
            .find(|known| under(&path, known))
            .map(PathBuf::as_path)
    }
}

/// 折成绝对且词法规范的路径。根要落盘，相对路径换个工作目录启动就指到别处去了。
fn normalise(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `child` 是否为 `parent` 本身或其子目录。
/// 按路径分量比而不是按字符串前缀比，`/music` 不该被当成 `/music-old` 的父目录。
fn under(child: &Path, parent: &Path) -> bool {
    let mut parts = child.components();
    for want in parent.components() {
        match parts.next() {
            Some(part) if same(part, want) => {}
            _ => return false,
        }
    }
    true
}

/// Windows 上盘符与目录名大小写不敏感，逐段比较时要折平。
fn same(left: Component<'_>, right: Component<'_>) -> bool {
    if cfg!(windows) {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::Roots;

    /// 期望值也要过一遍归一。Windows 上 `/music` 会折成 `D:\music`，直接比字面量测不到东西。
    fn want(paths: &[&str]) -> Vec<PathBuf> {
        paths
            .iter()
            .map(|p| super::normalise(Path::new(p)))
            .collect()
    }

    fn roots(paths: &[&str]) -> Vec<PathBuf> {
        let roots = Roots::from_paths(paths.iter().map(PathBuf::from));
        roots.items().to_vec()
    }

    #[test]
    fn 同一个目录只占一行() {
        let mut roots = Roots::new();
        assert!(roots.add("/music"));
        assert!(!roots.add("/music"), "第二次加同一个目录不该再多一行");
        assert!(!roots.add("/music/"), "尾部斜杠不该被当成另一个目录");
        assert!(!roots.add("/music/./"), "`.` 段要被折掉");
        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn 子目录不会重复占位() {
        assert_eq!(roots(&["/music", "/music/live"]), want(&["/music"]));
        assert_eq!(roots(&["/music/live", "/music"]), want(&["/music"]));
    }

    #[test]
    fn 兄弟目录各占一行且保留顺序() {
        assert_eq!(
            roots(&["/music/a", "/music/b", "/music/a"]),
            want(&["/music/a", "/music/b"])
        );
    }

    #[test]
    fn 前缀相同但不是子目录() {
        assert_eq!(
            roots(&["/music", "/music-old"]),
            want(&["/music", "/music-old"]),
            "`/music-old` 不是 `/music` 的子目录"
        );
    }

    #[test]
    fn 移除只认同一条() {
        let mut roots = Roots::from_paths(["/music".into(), "/music/live".into()]);
        // `/music/live` 已经被 `/music` 覆盖，不会单独入册。
        assert_eq!(roots.len(), 1);
        assert!(!roots.remove(Path::new("/music/live")));
        assert!(roots.remove(Path::new("/music")));
        assert!(roots.is_empty());
    }

    #[test]
    fn 归属判定按目录边界() {
        let roots = Roots::from_paths(["/music".into(), "/other".into()]);
        assert_eq!(
            roots.root_of(Path::new("/music/live/a.flac")),
            Some(super::normalise(Path::new("/music")).as_path())
        );
        assert_eq!(roots.root_of(Path::new("/music-old/a.flac")), None);
        assert_eq!(
            roots.root_of(Path::new("/music")),
            Some(super::normalise(Path::new("/music")).as_path())
        );
    }
}
