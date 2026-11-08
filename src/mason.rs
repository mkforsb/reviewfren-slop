//! Resolving and installing language servers from the Mason registry
//! (<https://github.com/mason-org/mason-registry>).

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum Server {
    RustAnalyzer,
    Gopls,
}

impl Server {
    pub const ALL: [Server; 2] = [Server::RustAnalyzer, Server::Gopls];

    /// Mason package name (also the binary name).
    pub fn name(self) -> &'static str {
        match self {
            Server::RustAnalyzer => "rust-analyzer",
            Server::Gopls => "gopls",
        }
    }

    pub fn handles(self, path: &str) -> bool {
        let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
        match self {
            Server::RustAnalyzer => ext == "rs",
            Server::Gopls => ext == "go" || path.ends_with("go.mod"),
        }
    }

    pub fn language_id(self) -> &'static str {
        match self {
            Server::RustAnalyzer => "rust",
            Server::Gopls => "go",
        }
    }
}

impl std::fmt::Display for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

fn mason_root() -> PathBuf {
    crate::settings::data_dir().join("mason")
}

fn nvim_mason_bin() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("nvim").join("mason").join("bin"))
}

fn is_executable(p: &Path) -> bool {
    p.is_file()
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| is_executable(p)))
}

/// Finds or installs the server binary. `progress` receives human readable status.
///
/// Order: our own Mason install dir, Neovim's Mason dir, fresh install from the Mason
/// registry, and finally whatever is on `PATH`.
pub async fn resolve(server: Server, progress: impl Fn(String)) -> Result<(PathBuf, String), String> {
    let own = mason_root().join("bin").join(server.name());
    if is_executable(&own) {
        return Ok((own, "Mason (app)".into()));
    }
    if let Some(nvim) = nvim_mason_bin().map(|d| d.join(server.name()))
        && is_executable(&nvim) {
            return Ok((nvim, "Mason (Neovim)".into()));
        }
    progress(format!("installing {} from Mason registry…", server.name()));
    match install(server, &progress).await {
        Ok(p) => Ok((p, "Mason (app, just installed)".into())),
        Err(e) => {
            eprintln!("[mason] install of {server} failed: {e}");
            if let Some(p) = which(server.name()) {
                progress(format!("Mason install failed, using {} from PATH", p.display()));
                Ok((p, "PATH (Mason install failed)".into()))
            } else {
                Err(format!("Could not install {server} from the Mason registry: {e}"))
            }
        }
    }
}

async fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .user_agent("reviewfren")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    resp.bytes().await.map(|b| b.to_vec()).map_err(|e| format!("GET {url}: {e}"))
}

/// Downloads (or reuses a cached copy younger than a day of) the registry.
async fn registry(progress: &impl Fn(String)) -> Result<Vec<Value>, String> {
    let cache = mason_root().join("registry.json.zip");
    let fresh = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().unwrap_or_default().as_secs() < 86_400)
        .unwrap_or(false);
    let bytes = if fresh {
        std::fs::read(&cache).map_err(|e| e.to_string())?
    } else {
        progress("downloading Mason registry…".into());
        let b = http_get("https://github.com/mason-org/mason-registry/releases/latest/download/registry.json.zip").await?;
        let _ = std::fs::create_dir_all(mason_root());
        let _ = std::fs::write(&cache, &b);
        b
    };
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut file = zip.by_name("registry.json").map_err(|e| e.to_string())?;
    let mut json = String::new();
    std::io::Read::read_to_string(&mut file, &mut json).map_err(|e| e.to_string())?;
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

fn current_targets() -> Vec<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => vec!["linux_x64_gnu", "linux_x64", "linux"],
        ("linux", "aarch64") => vec!["linux_arm64_gnu", "linux_arm64", "linux"],
        ("macos", "x86_64") => vec!["darwin_x64", "darwin"],
        ("macos", "aarch64") => vec!["darwin_arm64", "darwin"],
        _ => vec![],
    }
}

async fn install(server: Server, progress: &impl Fn(String)) -> Result<PathBuf, String> {
    let packages = registry(progress).await?;
    let pkg = packages
        .iter()
        .find(|p| p["name"] == server.name())
        .ok_or_else(|| format!("{server} not found in Mason registry"))?;
    let id = pkg["source"]["id"].as_str().ok_or("package has no source id")?;
    let (purl, version) = id.rsplit_once('@').ok_or("unversioned purl")?;
    let pkg_dir = mason_root().join("packages").join(server.name());
    std::fs::create_dir_all(&pkg_dir).map_err(|e| e.to_string())?;
    let bin_dir = mason_root().join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let link = bin_dir.join(server.name());

    let installed: PathBuf = if let Some(repo) = purl.strip_prefix("pkg:github/") {
        let targets = current_targets();
        let assets = pkg["source"]["asset"].as_array().ok_or("no assets in registry entry")?;
        let asset = targets
            .iter()
            .find_map(|t| assets.iter().find(|a| a["target"] == *t))
            .ok_or("no Mason asset for this platform")?;
        let file = asset["file"].as_str().ok_or("asset without file")?.replace("{{version}}", version);
        let bin = asset["bin"].as_str().unwrap_or(server.name()).replace("{{version}}", version);
        if !file.ends_with(".gz") || file.ends_with(".tar.gz") {
            return Err(format!("unsupported asset archive format: {file}"));
        }
        let url = format!("https://github.com/{repo}/releases/download/{version}/{file}");
        progress(format!("downloading {server} {version}…"));
        let gz = http_get(&url).await?;
        progress(format!("unpacking {server}…"));
        let mut decoder = flate2::read::GzDecoder::new(&gz[..]);
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut data).map_err(|e| e.to_string())?;
        let target = pkg_dir.join(&bin);
        std::fs::write(&target, data).map_err(|e| e.to_string())?;
        set_executable(&target)?;
        target
    } else if let Some(module) = purl.strip_prefix("pkg:golang/") {
        let go = which("go").ok_or("Mason installs gopls with `go install`, but `go` is not on PATH")?;
        progress(format!("go install {module}@{version}…"));
        let out = tokio::process::Command::new(go)
            .args(["install", &format!("{module}@{version}")])
            .env("GOBIN", &pkg_dir)
            .output()
            .await
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("go install failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        let name = module.rsplit('/').next().unwrap_or(server.name());
        pkg_dir.join(name)
    } else {
        return Err(format!("unsupported Mason source: {purl}"));
    };

    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&installed, &link).map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    std::fs::copy(&installed, &link).map_err(|e| e.to_string())?;
    let _ = std::fs::write(pkg_dir.join("VERSION"), version);
    Ok(link)
}

fn set_executable(p: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(())
}
