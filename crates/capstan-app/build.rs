//! 给 Windows 产物嵌入版本资源（`VERSIONINFO`）。

#[cfg(windows)]
use std::path::Path;

fn main() {
    // 产品版本或描述改了要重跑，否则 exe 上的数字会停在旧值。
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=assets/capstan.ico");

    #[cfg(windows)]
    embed_version_info();
}

#[cfg(windows)]
fn embed_version_info() {
    // 交叉编译到非 Windows 目标时不该塞 Windows 资源。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = product_version();
    let (major, minor, patch) = version_parts(&version);
    // 固定版本号是四个 16 位字段拼成的一个 u64。Explorer 读的是字符串字段，
    // 但升级安装、版本比较类工具读的是这个定长字段，两边都写才一致。
    let fixed = u64::from(major) << 48 | u64::from(minor) << 32 | u64::from(patch) << 16;

    let mut res = winresource::WindowsResource::new();
    // 决定属性页「语言」一栏，与下面字符串的内容无关。
    res.set_language(0x0804);
    res.set_icon("assets/capstan.ico");
    // `FileDescription` 是任务管理器、开始菜单、文件对话框里显示的名字。
    res.set("FileDescription", "Capstan Player");
    res.set("ProductName", "Capstan");
    res.set("CompanyName", "Capstan");
    // `WindowsResource::new()` 用 package 名预填了这两项，而包名是 `capstan-app`，
    // 实际产物叫 `capstan.exe`，不覆盖会对不上。
    res.set("InternalName", "capstan");
    res.set("OriginalFilename", "capstan.exe");
    res.set("FileVersion", &version);
    res.set("ProductVersion", &version);
    res.set("LegalCopyright", "Copyright © 2026 JDBeWL");

    res.set_version_info(winresource::VersionInfo::FILEVERSION, fixed);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, fixed);

    if let Err(err) = res.compile() {
        panic!("嵌入 Windows 版本资源失败（检查 Windows SDK 的 rc.exe 是否可用）：{err}");
    }
}

/// 读 workspace 根 `Cargo.toml` 里的产品版本。
#[cfg(windows)]
fn product_version() -> String {
    let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        panic!("CARGO_MANIFEST_DIR 未设置，无法定位 workspace 根 Cargo.toml");
    };
    // CARGO_MANIFEST_DIR 是 crates/capstan-app，上两级才是仓库根。
    let root = Path::new(&manifest_dir).join("../..").join("Cargo.toml");
    let text = match std::fs::read_to_string(&root) {
        Ok(text) => text,
        Err(err) => panic!("读不到 {}：{err}", root.display()),
    };
    let doc: toml::Value = match toml::from_str(&text) {
        Ok(doc) => doc,
        Err(err) => panic!("{} 解析失败：{err}", root.display()),
    };

    doc.get("workspace")
        .and_then(|workspace| workspace.get("metadata"))
        .and_then(|metadata| metadata.get("product"))
        .and_then(|product| product.get("version"))
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "{} 缺少 [workspace.metadata.product] version",
                root.display()
            )
        })
        .to_owned()
}

/// `0.4.0` / `0.4.0-rc.1` → `(0, 4, 0)`。固定版本号只认前三段，预发布后缀不带入。
#[cfg(windows)]
fn version_parts(version: &str) -> (u16, u16, u16) {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.').map(|part| part.parse::<u16>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}
