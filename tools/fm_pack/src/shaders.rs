// Feature shaders (shader pipeline plan, slice 4): each <source>/shaders/<name>.hlsl becomes the
// package file shaders/<name>.cso. The compiler is the pinned dxc of fm_render (its hashes come
// from crates/fm_render/shaders/manifest.json, so built-in and feature shaders use one compiler),
// with shader contract v1 (fm_shader.hlsli, from this build) on the include path. Pixel shaders
// only (ps_6_0, entry main); .hlsli files are an author's own includes and are not compiled.
use sha2::Digest;
use std::path::{Path, PathBuf};

const MANIFEST: &str = include_str!("../../../crates/fm_render/shaders/manifest.json");
const CONTRACT: &str = include_str!("../../../crates/fm_render/shaders/fm_shader.hlsli");
// The Windows tools, by full path: Git's tar reads "C:\..." as a remote host.
const CURL: &str = r"C:\Windows\System32\curl.exe";
const TAR: &str = r"C:\Windows\System32\tar.exe";

fn sha256(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The pinned compiler: the official DirectXShaderCompiler release in fm_render's manifest.json
/// (zip URL, zip SHA-256, file hashes). A Windows SDK copy differs between build computers (the
/// GitHub runner, 2026-10-26), the release does not. The first call downloads and checks the zip
/// into %LOCALAPPDATA%\fm_pack; each call checks the files again before use.
pub fn compiler() -> Result<PathBuf, String> {
    let cache = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).ok_or("LOCALAPPDATA is not set")?.join("fm_pack");
    compiler_in(&cache)
}

/// The compiler in `cache`. One file lock covers the check, the repair and the publish, so two
/// fm_pack processes never repair the same cache at once (review r1: one could delete the
/// other's folder). A good cache is never deleted, so a compile that runs after the lock is safe.
fn compiler_in(cache: &Path) -> Result<PathBuf, String> {
    let m: serde_json::Value = serde_json::from_str(MANIFEST).map_err(|e| e.to_string())?;
    let c = &m["compiler"];
    let (url, zip_sha) = (c["url"].as_str(), c["sha256"].as_str());
    let (Some(url), Some(zip_sha), Some(hashes)) = (url, zip_sha, c["hashes"].as_object()) else {
        return Err("fm_render manifest.json has no compiler url, sha256 and hashes".into());
    };
    let err = |p: &Path| { let p = p.display().to_string(); move |e: std::io::Error| format!("{p}: {e}") };
    std::fs::create_dir_all(cache).map_err(err(cache))?;
    let lock_path = cache.join("dxc.lock");
    let lock = std::fs::File::create(&lock_path).map_err(err(&lock_path))?;
    lock.lock().map_err(err(&lock_path))?;
    let name = format!("dxc-{}", &zip_sha[..16.min(zip_sha.len())]);
    let (dir, zip) = (cache.join(&name), cache.join(format!("{name}.zip")));
    let bin = dir.join("bin").join("x64");
    if pinned(&bin, hashes).is_err() {
        if dir.exists() { std::fs::remove_dir_all(&dir).map_err(err(&dir))?; }
        fetch(url, zip_sha, &zip, &dir)?;
        pinned(&bin, hashes)?;
    }
    Ok(bin.join("dxc.exe"))
}

/// Ok when each pinned file in `bin` has its pinned SHA-256.
fn pinned(bin: &Path, hashes: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    for (file, want) in hashes {
        let path = bin.join(file);
        let got = sha256(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?);
        if Some(got.as_str()) != want.as_str() { return Err(format!("{} is not the pinned compiler ({got})", path.display())); }
    }
    Ok(())
}

/// Unpacks the release zip into `dir` (the caller holds the cache lock). The zip stays in the
/// cache; it is downloaded only when it is missing or has another SHA-256, so a damaged unpack
/// is repaired with no network. The unpack goes to a work folder first, then one rename.
fn fetch(url: &str, want: &str, zip: &Path, dir: &Path) -> Result<(), String> {
    let run = |cmd: &mut std::process::Command| match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!("{:?} failed: {}", cmd, String::from_utf8_lossy(&o.stderr))),
        Err(e) => Err(format!("{:?}: {e}", cmd)),
    };
    let zip_ok = |z: &Path| std::fs::read(z).map(|b| sha256(&b) == want).unwrap_or(false);
    if !zip_ok(zip) {
        let part = zip.with_extension("zip.part");
        let _ = std::fs::remove_file(&part);
        run(std::process::Command::new(CURL).args(["-fsSL", "--retry", "3", "-o"]).arg(&part).arg(url))?;
        if !zip_ok(&part) { let _ = std::fs::remove_file(&part); return Err(format!("{url} is not the pinned zip (SHA-256 {want})")); }
        std::fs::rename(&part, zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    }
    let work = dir.with_extension("work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let result = run(std::process::Command::new(TAR).arg("-x").arg("-f").arg(zip).current_dir(&work))
        .and_then(|()| std::fs::rename(&work, dir).map_err(|e| format!("{}: {e}", dir.display())));
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// The compiled shaders of `dir`/shaders in name order: none when the folder does not exist.
/// A bad name or a compile error stops the pack with dxc's message.
pub fn compile(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let src = dir.join("shaders");
    if !src.is_dir() { return Ok(Vec::new()); }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&src).map_err(|e| format!("{}: {e}", src.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let file = path.file_name().and_then(|f| f.to_str()).unwrap_or_default().to_string();
        if file.ends_with(".hlsli") { continue; }
        let name = file.strip_suffix(".hlsl").filter(|n| fm_feature_sdk::shader_name_valid(n));
        names.push(name.map(String::from).ok_or_else(|| format!("{}: only <name>.hlsl (a-z, 0-9, _ or -) and .hlsli files", path.display()))?);
    }
    names.sort();
    if names.is_empty() { return Err(format!("{} holds no .hlsl shader", src.display())); }
    let dxc = compiler()?;
    let work = std::env::temp_dir().join(format!("fm-pack-shaders-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let result = (|| {
        std::fs::write(work.join("fm_shader.hlsli"), CONTRACT).map_err(|e| e.to_string())?;
        names.iter().map(|name| {
            let out = work.join(format!("{name}.cso"));
            let run = std::process::Command::new(&dxc)
                .args(["-nologo", "-T", "ps_6_0", "-E", "main", "-O3", "-Qstrip_debug", "-Qstrip_reflect", "-I"])
                .arg(&work).arg("-Fo").arg(&out).arg(src.join(format!("{name}.hlsl")))
                .output().map_err(|e| format!("{}: {e}", dxc.display()))?;
            if !run.status.success() {
                return Err(format!("shaders/{name}.hlsl does not compile:\n{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr)));
            }
            let bytes = std::fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            Ok((name.clone(), bytes))
        }).collect()
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Developer hot reload: compiles `dir`/shaders and writes each <name>.cso into `out`. Each file
/// goes to a temporary name first and then replaces the old one, so a host never reads half.
pub fn write(dir: &Path, out: &Path) -> Result<(), String> {
    let built = compile(dir)?;
    if built.is_empty() { return Err(format!("{} has no shaders/ folder", dir.display())); }
    for (name, bytes) in built {
        let (tmp, file) = (out.join(format!("{name}.cso.tmp")), out.join(format!("{name}.cso")));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &file).map_err(|e| format!("{}: {e}", file.display()))?;
        println!("{} ({} bytes)", file.display(), bytes.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const GOOD: &str = "#include \"fm_shader.hlsli\"\n#include \"common.hlsli\"\n[RootSignature(FM_RS)] float4 main(FmPixel i) : SV_Target { return half_of(fm_color.Sample(fm_point, i.uv)); }\n";

    // A changed or missing compiler file is refused, so a damaged cache is downloaded again.
    #[test]
    fn a_compiler_file_with_another_hash_is_refused() {
        let dir = std::env::temp_dir().join(format!("fm-pack-dxc-pin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dxc.exe"), b"good").unwrap();
        let mut hashes = serde_json::Map::new();
        hashes.insert("dxc.exe".into(), super::sha256(b"good").into());
        assert!(super::pinned(&dir, &hashes).is_ok());
        std::fs::write(dir.join("dxc.exe"), b"bad").unwrap();
        assert!(super::pinned(&dir, &hashes).unwrap_err().contains("is not the pinned compiler"));
        hashes.insert("dxil.dll".into(), super::sha256(b"x").into());
        std::fs::write(dir.join("dxc.exe"), b"good").unwrap();
        assert!(super::pinned(&dir, &hashes).is_err(), "a missing file is refused");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Review r1: fm_pack processes that find one damaged cache at the same time must all get the
    // compiler. Six copies of this test binary run `repair_child` on one cache with a bad dxc.exe
    // and the checked zip (copied from the real cache when it has one; else the first child
    // downloads it). Without the cache lock, one process deletes or replaces the folder of another.
    #[cfg(windows)]
    #[test]
    fn processes_that_repair_one_cache_at_once_all_succeed() {
        let real = super::compiler().unwrap();
        let real_dir = real.parent().and_then(std::path::Path::parent).and_then(std::path::Path::parent).unwrap();
        let cache = std::env::temp_dir().join(format!("fm-pack-dxc-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let dir = cache.join(real_dir.file_name().unwrap());
        std::fs::create_dir_all(dir.join("bin").join("x64")).unwrap();
        std::fs::write(dir.join("bin").join("x64").join("dxc.exe"), b"damaged").unwrap();
        if real_dir.with_extension("zip").is_file() {
            std::fs::copy(real_dir.with_extension("zip"), dir.with_extension("zip")).unwrap();
        }
        let exe = std::env::current_exe().unwrap();
        let running: Vec<_> = (0..6).map(|_| std::process::Command::new(&exe)
            .args(["--exact", "shaders::tests::repair_child", "--nocapture"])
            .env("FM_PACK_REPAIR_CACHE", &cache)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().unwrap()).collect();
        let children: Vec<_> = running.into_iter().map(|c| c.wait_with_output().unwrap()).collect();
        for c in &children {
            assert!(c.status.success(), "{}{}", String::from_utf8_lossy(&c.stdout), String::from_utf8_lossy(&c.stderr));
        }
        let m: serde_json::Value = serde_json::from_str(super::MANIFEST).unwrap();
        assert!(super::pinned(&dir.join("bin").join("x64"), m["compiler"]["hashes"].as_object().unwrap()).is_ok());
        let _ = std::fs::remove_dir_all(&cache);
    }

    // One repair process of the test above; it does nothing in a normal test run.
    #[test]
    fn repair_child() {
        let Some(cache) = std::env::var_os("FM_PACK_REPAIR_CACHE") else { return };
        let dxc = super::compiler_in(std::path::Path::new(&cache)).unwrap();
        assert!(dxc.is_file());
    }

    // A bad file name stops before the compiler, so this runs on every system.
    #[test]
    fn a_bad_shader_name_stops_before_the_compiler() {
        let dir = std::env::temp_dir().join(format!("fm-pack-shader-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shaders")).unwrap();
        std::fs::write(dir.join("shaders").join("Bad Name.hlsl"), GOOD).unwrap();
        assert!(super::compile(&dir).unwrap_err().contains("only <name>.hlsl"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A contract shader compiles to a DXBC container; a broken one stops with dxc's message. The
    // pinned compiler is the Windows build of the dxc release (downloaded once), so this runs on
    // Windows only.
    #[cfg(windows)]
    #[test]
    fn feature_shaders_compile_with_the_pinned_compiler() {
        let dir = std::env::temp_dir().join(format!("fm-pack-shader-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shaders")).unwrap();
        let good = GOOD;
        std::fs::write(dir.join("shaders").join("half.hlsl"), good).unwrap();
        std::fs::write(dir.join("shaders").join("common.hlsli"), "float4 half_of(float4 c) { return c * fm_params[0].x; }\n").unwrap();
        let built = super::compile(&dir).unwrap();
        assert_eq!(built.len(), 1);
        assert_eq!((built[0].0.as_str(), &built[0].1[..4]), ("half", b"DXBC".as_slice()));
        std::fs::write(dir.join("shaders").join("broken.hlsl"), "float4 main() : SV_Target { return nothing; }\n").unwrap();
        let e = super::compile(&dir).unwrap_err();
        assert!(e.contains("shaders/broken.hlsl does not compile") && e.contains("nothing"), "{e}");
        std::fs::remove_file(dir.join("shaders").join("broken.hlsl")).unwrap();
        std::fs::write(dir.join("shaders").join("Bad Name.hlsl"), good).unwrap();
        assert!(super::compile(&dir).unwrap_err().contains("only <name>.hlsl"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
