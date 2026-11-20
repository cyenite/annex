//! microG, the stand-in's Google Play services: GmsCore and its Companion, downloaded from microG's
//! GitHub release and pinned by SHA-256. GmsCore gets offline push tokens
//! (`android/play-services/AnnexPush.smali`) and maps from OpenFreeMap instead of Stadia Maps with
//! microG's key, and both are re-signed with a key made on this machine, because they share
//! signature permissions.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{APKTOOL, Tool, fetch, keep, root, run_java, smali};
use crate::paths::write_if_changed;

pub const HELPER: &str = include_str!("../../android/play-services/AnnexPush.smali");
const SIGNER: &[u8] = include_bytes!("../../android/play-services/annex-sign.jar");
const VERSION: &str = "0.3.17.252432";
const RELEASES: &str = "https://github.com/microg/GmsCore/releases/download/v0.3.17.252432";
// Bump when the helper, the hooks or the map style change, so instances get a freshly patched
// GmsCore.
const PATCH_VERSION: &str = "2";
const APKSIG: Tool = Tool {
    file: "apksig-9.4.1.jar",
    url: "https://dl.google.com/android/maven2/com/android/tools/build/apksig/9.4.1/apksig-9.4.1.jar",
    sha256: "7ae2e5980c77d853e3513074ee7c822bbdcdcde1668889d17faa7fe8bc8aa821",
};
// The key file is private to the user; the password only satisfies PKCS12.
const KEY_PASSWORD: &str = "annex-stand-in";
const KEY: &str = "stand-in.p12";
const HELPER_CLASS: &str = "dev/annex/AnnexPush.smali";
const STYLES_CODE: &str = "org/microg/gms/maps/mapbox/StylesKt.smali";
const NORMAL_STYLE: &str = "assets/style-microg-normal-stadia.json";
// microG picks one of these by map type while it has a Stadia key. All three get the keyless
// normal map: no keyless source has satellite imagery, and the outdoors style loads its fonts and
// icons from Stadia too.
const STYLES: [&str; 3] = [
    NORMAL_STYLE,
    "assets/style-microg-satellite-stadia.json",
    "assets/style-stadia-outdoors.json",
];
const TILES_SOURCE: &str = "openmaptiles";
// Free, keyless and in the OpenMapTiles schema microG's style is drawn for.
const OPENFREEMAP: &str = "https://tiles.openfreemap.org/planet";
const KEY_SUFFIX: &str = ", \"?api_key=";
const REGISTER_SERVICE: &str = "org/microg/gms/gcm/PushRegisterServiceKt.smali";
const REGISTER_MANAGER: &str = "org/microg/gms/gcm/PushRegisterManager.smali";
const CHECKIN: &str = ".method private static final ensureCheckinIsUpToDate(Landroid/content/Context;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;";
const ALLOWED: &str = ".method public static final ensureAppRegistrationAllowed(Landroid/content/Context;Lorg/microg/gms/gcm/GcmDatabase;Ljava/lang/String;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;";
const COMPLETE: &str = ".method public static completeRegisterRequest(Landroid/content/Context;Lorg/microg/gms/gcm/GcmDatabase;Ljava/lang/String;Lorg/microg/gms/gcm/RegisterRequest;Lorg/microg/gms/gcm/PushRegisterManager$BundleCallback;)V";
const IS_OFFLINE: &str = "Ldev/annex/AnnexPush;->isOffline(Landroid/content/Context;)Z";
const ANSWER: &str = "Ldev/annex/AnnexPush;->complete(Landroid/content/Context;Ljava/lang/Object;Ljava/lang/String;Ljava/lang/Object;Ljava/lang/Object;)Z";

/// A microG release file, mounted as a privileged system app: the stand-in needs privileged
/// permissions, and only those folders may borrow Google's certificate (`AnnexSignatures`).
pub struct StandInApk {
    pub asset: &'static str,
    pub sha256: &'static str,
    pub target: &'static str,
    /// Only GmsCore gets the push handler; Companion is just re-signed to match it.
    pub is_patched: bool,
}

pub const APKS: [StandInApk; 2] = [
    StandInApk {
        asset: "com.google.android.gms-252432034.apk",
        sha256: "e5e6cba078bdec812b5450599350e5c1a12c8b7438da5bf77123af170f8c1a35",
        target: "/system/priv-app/GmsCore/GmsCore.apk",
        is_patched: true,
    },
    StandInApk {
        asset: "com.android.vending-84022634.apk",
        sha256: "9b8616011cc8f026bcbbe094df850794abb9d6d9f23214d7a8f8c4d1033ea012",
        target: "/system/priv-app/Phonesky/Phonesky.apk",
        is_patched: false,
    },
];

impl StandInApk {
    fn file_name(&self) -> &'static str {
        self.target
            .rsplit('/')
            .next()
            .expect("a target has a file name")
    }
}

/// The patched, re-signed APKs, in `APKS` order, built on first use.
pub fn patched() -> Result<Vec<PathBuf>> {
    let folder = root().join("microg").join(VERSION);
    let built_folder = folder.join(format!("annex-{PATCH_VERSION}"));
    let built: Vec<PathBuf> = APKS
        .iter()
        .map(|apk| built_folder.join(apk.file_name()))
        .collect();
    if built.iter().all(|path| path.is_file()) {
        return Ok(built);
    }
    downloaded(&folder)?;
    println!("preparing the Google services stand-in (once, about two minutes)");
    let work = root().join(format!(".work-microg-{}", process::id()));
    fs::create_dir_all(&work)?;
    let result = build(&folder, &work, &built);
    let _ = fs::remove_dir_all(&work);
    result.context("could not prepare the Google services stand-in")?;
    Ok(built)
}

/// Downloads any of microG's release files missing from `folder`, checking each one's hash.
fn downloaded(folder: &Path) -> Result<()> {
    if APKS.iter().all(|apk| folder.join(apk.asset).is_file()) {
        return Ok(());
    }
    println!("downloading the Google services stand-in (about 110 MB, first time only)");
    for apk in &APKS {
        let url = format!("{RELEASES}/{}", apk.asset);
        fetch(&url, apk.sha256, &folder.join(apk.asset))?;
    }
    Ok(())
}

fn build(upstream: &Path, work: &Path, built: &[PathBuf]) -> Result<()> {
    let key = signing_key()?;
    let tools = root().join("tools");
    write_if_changed(&tools.join("annex-sign.jar"), SIGNER, 0o644)?;
    let apktool = APKTOOL.inside();
    let decoded = "/work/gmscore";
    let unsigned = "/work/gmscore-unsigned.apk";
    let patched_apk = APKS
        .iter()
        .find(|apk| apk.is_patched)
        .expect("GmsCore is patched");
    let input = format!("/input/{}", patched_apk.asset);
    let keys = key.parent().expect("the key has a folder");
    let volumes = [(upstream, "/input:ro"), (work, "/work"), (keys, "/keys:ro")];
    // Without resource decoding (-r) apktool copies them untouched, so only code is rebuilt.
    run_java(
        &[&APKTOOL],
        &volumes,
        &[
            "java", "-jar", &apktool, "d", "-r", "-f", "-o", decoded, &input,
        ],
    )?;
    patch(&work.join("gmscore"))?;
    run_java(
        &[&APKTOOL],
        &volumes,
        &["java", "-jar", &apktool, "b", "-o", unsigned, decoded],
    )?;
    let classpath = format!("/tools/annex-sign.jar:{}", APKSIG.inside());
    let keystore = format!("/keys/{KEY}");
    for (apk, cached) in APKS.iter().zip(built) {
        let source = if apk.is_patched {
            unsigned.to_string()
        } else {
            format!("/input/{}", apk.asset)
        };
        let signed = format!("/work/{}", apk.file_name());
        run_java(
            &[&APKSIG],
            &volumes,
            &[
                "java",
                "-cp",
                &classpath,
                "AnnexSign",
                &keystore,
                KEY_PASSWORD,
                &source,
                &signed,
            ],
        )?;
        keep(&work.join(apk.file_name()), cached)?;
    }
    Ok(())
}

/// This machine's key for the stand-in, made on first use. Each machine has its own, so nobody
/// else can sign an update that replaces an instance's GmsCore.
fn signing_key() -> Result<PathBuf> {
    let folder = root();
    let key = folder.join(KEY);
    if key.is_file() {
        return Ok(key);
    }
    fs::create_dir_all(&folder)?;
    let staged = format!(".{KEY}.new");
    let _ = fs::remove_file(folder.join(&staged));
    let output = format!("/keys/{staged}");
    run_java(
        &[],
        &[(folder.as_path(), "/keys")],
        &[
            "keytool",
            "-genkeypair",
            "-keystore",
            &output,
            "-storetype",
            "PKCS12",
            "-storepass",
            KEY_PASSWORD,
            "-alias",
            "stand-in",
            "-keyalg",
            "RSA",
            "-keysize",
            "3072",
            "-validity",
            "36500",
            "-dname",
            "CN=annex Google services stand-in",
        ],
    )
    .context("could not make the stand-in's signing key")?;
    let staged = folder.join(staged);
    fs::set_permissions(&staged, Permissions::from_mode(0o600))?;
    fs::rename(&staged, &key)?;
    Ok(key)
}

fn patch(decoded: &Path) -> Result<()> {
    let folders = smali::folders(decoded)?;
    let service = smali::class(&folders, REGISTER_SERVICE)?;
    let source = fs::read_to_string(&service)?;
    let source = smali::guard_entry(&source, CHECKIN, skip_when_offline)?;
    let source = smali::guard_entry(&source, ALLOWED, skip_when_offline)?;
    fs::write(&service, source)?;
    let manager = smali::class(&folders, REGISTER_MANAGER)?;
    let source = smali::guard_entry(
        &fs::read_to_string(&manager)?,
        COMPLETE,
        answer_when_offline,
    )?;
    fs::write(&manager, source)?;
    smali::add_class(&folders, HELPER_CLASS, HELPER)?;
    let styles_code = smali::class(&folders, STYLES_CODE)?;
    fs::write(
        &styles_code,
        without_key(&fs::read_to_string(&styles_code)?)?,
    )?;
    let style = keyless_style(&fs::read_to_string(decoded.join(NORMAL_STYLE))?)?;
    for file in STYLES {
        fs::write(decoded.join(file), &style)?;
    }
    Ok(())
}

/// microG's normal style with its vector tiles from OpenFreeMap, and without the layers drawn from
/// Stadia's own landcover and elevation sources, which nothing keyless replaces.
fn keyless_style(source: &str) -> Result<String> {
    let mut style: Value = serde_json::from_str(source).context("the map style isn't JSON")?;
    let sources = style["sources"]
        .as_object_mut()
        .context("the map style has no sources")?;
    let tiles = sources
        .get_mut(TILES_SOURCE)
        .context("the map style has no openmaptiles source")?;
    *tiles = json!({ "type": "vector", "url": OPENFREEMAP });
    sources.retain(|name, _| name == TILES_SOURCE);
    let layers = style["layers"]
        .as_array_mut()
        .context("the map style has no layers")?;
    layers.retain(|layer| {
        layer
            .get("source")
            .is_none_or(|source| source == TILES_SOURCE)
    });
    Ok(style.to_string())
}

/// Empties the `?api_key=...` suffix getStyle adds to every source, so microG's Stadia key isn't
/// sent to OpenFreeMap. The key itself stays, because getStyle only picks the Stadia style files,
/// which `keyless_style` replaced, while it has one.
fn without_key(smali: &str) -> Result<String> {
    let suffixes = smali
        .lines()
        .filter(|line| line.trim_start().starts_with("const-string ") && line.contains(KEY_SUFFIX))
        .count();
    if suffixes != 1 {
        bail!("expected one map key suffix in getStyle, found {suffixes}");
    }
    let lines = smali
        .lines()
        .map(|line| match line.split_once(KEY_SUFFIX) {
            Some((instruction, _)) if line.trim_start().starts_with("const-string ") => {
                format!("{instruction}, \"\"")
            }
            _ => line.to_string(),
        })
        .collect();
    Ok(smali::finish(lines, smali))
}

/// For the two suspend checks that refuse registration while checkin is off: finish at once, as
/// they do when everything is in order, by returning Unit.
fn skip_when_offline(register: &str) -> Vec<String> {
    vec![
        format!("invoke-static/range {{p0 .. p0}}, {IS_OFFLINE}"),
        format!("move-result {register}"),
        format!("if-eqz {register}, {}", smali::CONTINUE),
        format!("sget-object {register}, Lkotlin/Unit;->INSTANCE:Lkotlin/Unit;"),
        format!("return-object {register}"),
    ]
}

/// For completeRegisterRequest: let AnnexPush answer with a placeholder token while checkin is off.
fn answer_when_offline(register: &str) -> Vec<String> {
    vec![
        format!("invoke-static/range {{p0 .. p4}}, {ANSWER}"),
        format!("move-result {register}"),
        format!("if-eqz {register}, {}", smali::CONTINUE),
        "return-void".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_apk_has_its_own_system_app_folder() {
        for apk in &APKS {
            assert_eq!(apk.sha256.len(), 64);
            assert!(apk.asset.ends_with(".apk"));
            assert!(apk.target.starts_with("/system/priv-app/"));
            assert!(apk.file_name().ends_with(".apk"));
        }
        assert_ne!(APKS[0].target, APKS[1].target);
        assert_eq!(APKS.iter().filter(|apk| apk.is_patched).count(), 1);
    }

    #[test]
    fn the_helper_has_the_methods_the_guards_call() {
        assert!(HELPER.starts_with(".class public final Ldev/annex/AnnexPush;"));
        for call in [IS_OFFLINE, ANSWER] {
            let signature = call.split("->").nth(1).unwrap();
            assert!(
                HELPER.contains(&format!(".method public static {signature}")),
                "{call}"
            );
        }
    }

    #[test]
    fn the_map_style_draws_only_from_openfreemap() {
        let stadia = r#"{
            "version": 8,
            "glyphs": "asset://{fontstack}/{range}.pbf",
            "sources": {
                "openmaptiles": {"type": "vector", "url": "https://tiles.stadiamaps.com/data/openmaptiles.json"},
                "terrain": {"type": "raster-dem", "tiles": ["https://tiles.stadiamaps.com/data/terrarium/{z}/{x}/{y}.png"]}
            },
            "layers": [
                {"id": "background", "type": "background"},
                {"id": "hillshade", "type": "hillshade", "source": "terrain"},
                {"id": "water", "type": "fill", "source": "openmaptiles", "source-layer": "water"}
            ]
        }"#;
        let style: Value = serde_json::from_str(&keyless_style(stadia).unwrap()).unwrap();
        assert_eq!(
            style["sources"],
            json!({"openmaptiles": {"type": "vector", "url": OPENFREEMAP}})
        );
        let ids: Vec<&str> = style["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| layer["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["background", "water"]);
        assert_eq!(style["glyphs"], "asset://{fontstack}/{range}.pbf");
        assert!(keyless_style(r#"{"sources": {}, "layers": []}"#).is_err());
    }

    #[test]
    fn removes_only_the_key_suffix() {
        let code = "    const-string v3, \"b599\"\n\n    const-string v8, \"?api_key=b599\"\n";
        assert_eq!(
            without_key(code).unwrap(),
            "    const-string v3, \"b599\"\n\n    const-string v8, \"\"\n"
        );
        assert!(without_key("    const-string v3, \"b599\"\n").is_err());
        let twice = format!("{code}{code}");
        assert!(without_key(&twice).is_err());
    }

    #[test]
    fn guards_skip_and_answer_through_the_helper() {
        let skip = skip_when_offline("v10").join("\n");
        assert!(skip.contains("move-result v10\nif-eqz v10, :annex_continue"));
        assert!(skip.ends_with("return-object v10"));
        let answer = answer_when_offline("v6").join("\n");
        assert!(answer.starts_with("invoke-static/range {p0 .. p4}"));
        assert!(answer.ends_with("return-void"));
    }
}
