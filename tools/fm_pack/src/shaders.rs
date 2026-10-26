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
    let m: serde_json::Value = serde_json::from_str(MANIFEST).map_err(|e| e.to_string())?;
    let c = &m["compiler"];
    let (url, zip_sha) = (c["url"].as_str(), c["sha256"].as_str());
    let (Some(url), Some(zip_sha), Some(hashes)) = (url, zip_sha, c["hashes"].as_object()) else {
        return Err("fm_render manifest.json has no compiler url, sha256 and hashes".into());
    };
    let cache = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).ok_or("LOCALAPPDATA is not set")?.join("fm_pack");
    let dir = cache.join(format!("dxc-{}", &zip_sha[..16.min(zip_sha.len())]));
    let bin = dir.join("bin").join("x64");
    if dir.exists() && pinned(&bin, hashes).is_err() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if !dir.exists() { fetch(url, zip_sha, &cache, &dir)?; }
    pinned(&bin, hashes)?;
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

/// Downloads the release zip, checks its SHA-256, unpacks it in a folder of this process, then
/// renames that folder to `dir` (one step, so a parallel fm_pack never sees half a compiler).
fn fetch(url: &str, want: &str, cache: &Path, dir: &Path) -> Result<(), String> {
    let work = cache.join(format!("download-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("x")).map_err(|e| format!("{}: {e}", work.display()))?;
    let zip = work.join("dxc.zip");
    let result = (|| {
        let run = |cmd: &mut std::process::Command| match cmd.output() {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(format!("{:?} failed: {}", cmd, String::from_utf8_lossy(&o.stderr))),
            Err(e) => Err(format!("{:?}: {e}", cmd)),
        };
        run(std::process::Command::new(CURL).args(["-fsSL", "--retry", "3", "-o"]).arg(&zip).arg(url))?;
        let got = sha256(&std::fs::read(&zip).map_err(|e| format!("{}: {e}", zip.display()))?);
        if got != want { return Err(format!("{url} has SHA-256 {got}, not the pinned {want}")); }
        run(std::process::Command::new(TAR).arg("-x").arg("-f").arg(&zip).current_dir(work.join("x")))?;
        // Another fm_pack can rename first: then its folder is the compiler, and pinned() checks it.
        if std::fs::rename(work.join("x"), dir).is_err() && !dir.exists() {
            return Err(format!("{}: cannot place the compiler", dir.display()));
        }
        Ok(())
    })();
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
