//! `cargo xtask packages`: the packages the editor's extensions download, built from their sources by the zig projects
//! in `packages/` - a project a package (`packages/<package>/`), building its port (`packages/ports/`) - and laid out as
//! the releases of the editor's repository hold them: a release a package version, `<package>-<version>-<release>`.
//!
//! 1. Zig, at the version `packages/packages.toml` pins, fetched as ziglang.org asks of automation: from a community
//!    mirror chosen at random (`?source=` naming us), its minisign signature verified with Zig's key and the file it
//!    was signed for held to the one asked for, then its SHA-256; ziglang.org itself last. Kept in
//!    `target/packages/zig/`.
//! 2. For each package asked for (all, where none is), its project built for each platform, into
//!    `target/packages/build/<package>/<platform>/`, with nothing it fetched before: what it fetches then is exactly
//!    what it is built from. On the platform it runs on, the preprocessor built is held to clang's (`check_cpp`). The
//!    projects are built in a copy of `packages/` (`target/packages/projects/`, `mirror`), where zig makes what it
//!    makes in a project's folder.
//! 3. What it fetched resolved to its components (`packages/components.toml`), by the hashes its manifests declare:
//!    a source no entry describes, or an entry whose URL does not name its version, fails the task by name.
//! 4. Each platform's build packed into `<package>-<version>-<platform>.zip`, deterministically - entries sorted, no
//!    times, Unix permissions kept - so that building again gives the same bytes; with `licenses/<component>/`, the
//!    license files of every component linked into it there.
//! 5. What the licenses ask of whoever ships the package, and what it takes to build it again, in `sources/`: each
//!    component's source as zig keeps it, named `<name>-<version>.tar.gz`; the `packages/` folder
//!    (`pe2-editor-packages.zip`); the zig used.
//! 6. `NOTES.md` saying what the package is and what it is made of, `setup.toml`, the `[[setup]]` entry of each archive
//!    as an extension's manifest takes it once published, and `SHA256SUMS` of all of it.
//!
//! Each release in `target/packages/<tag>/`. `--zig <path>` builds with another zig, for trying one: what it makes is
//! not what the pins say, and says so in its notes. `--publish` makes each release one of the repository
//! `packages.toml` names, with `gh`: its notes the page, every file an asset - which nobody should do but who means to
//! publish it, never of a build with another zig, and never over a release already published (a package built anew
//! at the same version is a new `release` of it in `packages.toml`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail, ensure};
use sha2::{Digest, Sha256};

pub const USAGE: &str =
    "cargo xtask packages [--package <package>]... [--platform <platform>]... [--zig <zig>] [--publish]";

/// What is asked of the task.
struct Options {
    /// The packages built; all `packages.toml` describes where none is given.
    packages: Vec<String>,
    /// The platforms built for; all `packages.toml` names where none is given.
    platforms: Vec<String>,
    /// A zig to build with in place of the pinned one.
    zig: Option<PathBuf>,
    /// The releases published once built.
    publish: bool,
}

/// The zig `packages.toml` pins.
struct Zig {
    version: String,
    /// Where the list of community mirrors is read.
    mirrors: String,
    /// Zig's minisign key, as ziglang.org gives it.
    public_key: String,
    /// What a mirror is told the download is for.
    source: String,
    /// For each machine it can run on (`linux-x86_64`), its file and that file's SHA-256.
    hosts: BTreeMap<String, (String, String)>,
}

/// A package, as `packages.toml` describes it.
struct Package {
    version: String,
    /// Which build of that version: a release is `<package>-<version>-<release>`.
    release: u32,
    /// What an extension's `[[setup]]` entry calls it.
    name: String,
    description: String,
    license: String,
    copyright: String,
    upstream: String,
    /// The programs an extension takes of it, by their names without `.exe`, each with the name it is placed under.
    take: Vec<(String, String)>,
}

impl Package {
    /// Its release's tag, and folder's name.
    fn tag(&self, name: &str) -> String {
        format!("{name}-{}-{}", self.version, self.release)
    }
}

/// An archive made of a package for a platform.
struct Archive {
    platform: String,
    path: PathBuf,
    /// The folder the package was built into.
    built: PathBuf,
}

/// `packages/packages.toml`.
struct Config {
    zig: Zig,
    /// The repository whose releases hold the archives.
    repository: String,
    /// Each platform built for, with the target zig builds it as (`x86_64-linux-musl`).
    platforms: BTreeMap<String, String>,
    packages: BTreeMap<String, Package>,
}

/// A source the packages are built from, as `packages/components.toml` describes it.
#[derive(Clone)]
struct Component {
    /// The URL a manifest fetches it from; none for the zig that builds.
    url: Option<String>,
    name: String,
    version: String,
    license: String,
    license_files: Vec<String>,
    upstream: String,
    /// Whether its code is in what ships, rather than only used to build it.
    linked: bool,
    /// Where it is linked, when not on every platform.
    platforms: Option<Vec<String>>,
    /// Which packages it is linked into, when not every one fetching it.
    packages: Option<Vec<String>>,
}

impl Component {
    fn linked_into(&self, package: &str, platform: &str) -> bool {
        self.linked
            && self.platforms.as_ref().is_none_or(|p| p.iter().any(|p| p == platform))
            && self.packages.as_ref().is_none_or(|p| p.iter().any(|p| p == package))
    }
}

/// A component a package was built from: where its source is unpacked (zig's own folder for zig), and its hash in
/// zig's cache (none for zig).
struct Used {
    component: Component,
    unpacked: PathBuf,
    hash: Option<String>,
}

/// The task: each release built and laid out, its folder said as it is done.
///
/// # Errors
/// What it is asked does not read, zig cannot be had, a build fails, a source is not described, or what it writes
/// cannot be written.
pub fn run(args: impl Iterator<Item = String>, workspace: &Path, target: &Path) -> Result<()> {
    let options = options(args)?;
    if options.publish {
        ensure!(options.zig.is_none(), "what another zig built is not published");
        ensure!(options.platforms.is_empty(), "a release published is built for every platform");
    }
    let packages = workspace.join("packages");
    let config = Config::read(&packages.join("packages.toml"))?;
    let components = read_components(&packages.join("components.toml"))?;
    let work = target.join("packages");
    let platforms: Vec<(String, String)> = if options.platforms.is_empty() {
        config.platforms.iter().map(|(p, t)| (p.clone(), t.clone())).collect()
    } else {
        let named = options.platforms.iter().map(|p| {
            let triple =
                config.platforms.get(p).with_context(|| format!("packages.toml builds for no platform {p}"))?;
            Ok((p.clone(), triple.clone()))
        });
        named.collect::<Result<_>>()?
    };
    let names: Vec<String> =
        if options.packages.is_empty() { config.packages.keys().cloned().collect() } else { options.packages.clone() };
    for name in &names {
        ensure!(config.packages.contains_key(name), "packages.toml describes no package {name}");
    }
    let (zig, tarball) = if let Some(zig) = &options.zig {
        (zig.clone(), None)
    } else {
        let (zig, tarball) = pinned_zig(&config.zig, &packages, &work.join("zig"))?;
        (zig, Some(tarball))
    };
    let here = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let copy = work.join("projects");
    mirror(&packages, &copy)?;
    let date = commit_time(&packages)?;
    for name in &names {
        let package = &config.packages[name];
        let tag = package.tag(name);
        let project = copy.join(name);
        ensure!(project.join("build.zig").is_file(), "no project of {name} ({})", project.display());
        // What zig fetched for the project, made again by this build alone: what it keeps there is what the package
        // is built from.
        let fetched = project.join("zig-pkg");
        if fetched.exists() {
            std::fs::remove_dir_all(&fetched).with_context(|| fetched.display().to_string())?;
        }
        let release = work.join(&tag);
        if release.exists() {
            std::fs::remove_dir_all(&release).with_context(|| release.display().to_string())?;
        }
        std::fs::create_dir_all(release.join("sources")).with_context(|| release.display().to_string())?;
        let mut built = Vec::new();
        for (platform, triple) in &platforms {
            let out = build(&zig, &project, &work, &date, name, platform, triple)?;
            if name == "aro-cpp" && *platform == here {
                let cpp = out.join(format!("cpp{}", std::env::consts::EXE_SUFFIX));
                check_cpp(&zig, &cpp, &project.join("tests"), &work.join("cpp-check"))?;
            }
            let path = release.join(format!("{name}-{}-{platform}.zip", package.version));
            built.push(Archive { platform: platform.clone(), path, built: out });
        }
        let zig_dir = zig.parent().context("zig is in a folder")?;
        let used = resolve(&components, &copy, &project, zig_dir)?;
        for archive in &built {
            pack(name, archive, &used)?;
        }
        sources(&packages, &work.join("global"), tarball.as_deref(), &used, &release.join("sources"))?;
        let zig_file = tarball.as_deref().and_then(Path::file_name).map(|f| f.to_string_lossy().into_owned());
        let notes = notes(&config, name, &tag, &built, &used, zig_file.as_deref());
        std::fs::write(release.join("NOTES.md"), notes).context("NOTES.md")?;
        let setup = setup(&config, name, &tag, &built)?;
        std::fs::write(release.join("setup.toml"), setup).context("setup.toml")?;
        let sums = sums(&release)?;
        std::fs::write(release.join("SHA256SUMS"), sums).context("SHA256SUMS")?;
        if options.publish {
            publish(&config, &tag, &release)?;
        }
        println!("{}", release.display());
    }
    Ok(())
}

/// A package's release made on the repository, every file of its folder an asset by its name.
fn publish(config: &Config, tag: &str, release: &Path) -> Result<()> {
    let repository = config
        .repository
        .strip_prefix("https://github.com/")
        .with_context(|| format!("{} is no GitHub repository", config.repository))?;
    // A release published is never replaced: what was downloaded from it is what its entries' hashes say.
    let published = Command::new("gh")
        .args(["release", "view", tag, "--repo", repository])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("gh does not run")?;
    ensure!(
        !published.success(),
        "{tag} is published already: a package built anew is a new `release` of it in packages.toml"
    );
    let mut files = Vec::new();
    crate::gather(release, "", &mut files)?;
    let mut assets: Vec<PathBuf> =
        files.iter().map(|(name, _, _)| release.join(name.trim_start_matches('/'))).collect();
    assets.retain(|asset| asset.file_name().is_some_and(|name| name != "NOTES.md"));
    assets.sort();
    let status = Command::new("gh")
        .args(["release", "create", tag, "--repo", repository, "--title", tag, "--notes-file"])
        .arg(release.join("NOTES.md"))
        .args(&assets)
        .status()
        .context("gh does not run")?;
    ensure!(status.success(), "gh could not publish {tag}");
    Ok(())
}

fn options(mut args: impl Iterator<Item = String>) -> Result<Options> {
    let mut options = Options { packages: Vec::new(), platforms: Vec::new(), zig: None, publish: false };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--package" => options.packages.push(args.next().context("--package names one, as ffmpeg")?),
            "--platform" => options.platforms.push(args.next().context("--platform names one, as linux-x86_64")?),
            "--zig" => options.zig = Some(args.next().context("--zig names a zig")?.into()),
            "--publish" => options.publish = true,
            other => bail!("no option {other}\n{USAGE}"),
        }
    }
    Ok(options)
}

impl Config {
    fn read(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
        let table: toml::Table = text.parse().with_context(|| path.display().to_string())?;
        let text_of = |table: &toml::Table, key: &str| -> Result<String> {
            let value = table.get(key).and_then(toml::Value::as_str);
            Ok(value.with_context(|| format!("{}: no `{key}`", path.display()))?.to_string())
        };
        let section = |key: &str| table.get(key).and_then(toml::Value::as_table);
        let zig = section("zig").with_context(|| format!("{}: no [zig]", path.display()))?;
        let mut hosts = BTreeMap::new();
        for (host, entry) in zig.get("host").and_then(toml::Value::as_table).into_iter().flatten() {
            let entry = entry.as_table().with_context(|| format!("[zig.host.{host}] is no table"))?;
            hosts.insert(host.clone(), (text_of(entry, "file")?, text_of(entry, "sha256")?));
        }
        let release = section("release").with_context(|| format!("{}: no [release]", path.display()))?;
        let repository = text_of(release, "repository")?;
        let zig = Zig {
            version: text_of(zig, "version")?,
            mirrors: text_of(zig, "mirrors")?,
            public_key: text_of(zig, "public_key")?,
            source: text_of(zig, "source")?,
            hosts,
        };
        let mut platforms = BTreeMap::new();
        for (platform, triple) in section("platform").into_iter().flatten() {
            let triple = triple.as_str().with_context(|| format!("[platform] {platform} is no target"))?;
            platforms.insert(platform.clone(), triple.to_string());
        }
        let mut packages = BTreeMap::new();
        for (name, package) in section("package").into_iter().flatten() {
            let package = package.as_table().with_context(|| format!("[package.{name}] is no table"))?;
            packages.insert(
                name.clone(),
                Package {
                    version: text_of(package, "version")?,
                    release: package
                        .get("release")
                        .and_then(toml::Value::as_integer)
                        .and_then(|r| u32::try_from(r).ok())
                        .with_context(|| format!("[package.{name}]: no `release`, a number"))?,
                    name: text_of(package, "name")?,
                    description: text_of(package, "description")?,
                    license: text_of(package, "license")?,
                    copyright: text_of(package, "copyright")?,
                    upstream: text_of(package, "upstream")?,
                    take: take(package.get("take")).with_context(|| format!("[package.{name}]: `take`"))?,
                },
            );
        }
        Ok(Config { zig, repository, platforms, packages })
    }
}

/// What a package's `take` says: a list of programs placed under their names, or a table of each program and the
/// name it is placed under.
fn take(value: Option<&toml::Value>) -> Result<Vec<(String, String)>> {
    match value.context("missing")? {
        toml::Value::Array(programs) => programs
            .iter()
            .map(|p| p.as_str().map(|p| (p.to_string(), p.to_string())).context("a program is named by a string"))
            .collect(),
        toml::Value::Table(programs) => programs
            .iter()
            .map(|(p, to)| Ok((p.clone(), to.as_str().context("a program is placed under a name")?.to_string())))
            .collect(),
        _ => bail!("neither a list of programs nor a table of them"),
    }
}

/// The pinned zig for this machine, fetched and checked where it is not kept yet: the program, and its tarball.
fn pinned_zig(zig: &Zig, project: &Path, kept: &Path) -> Result<(PathBuf, PathBuf)> {
    let host = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let (file, sha256) = zig.hosts.get(&host).with_context(|| format!("packages.toml pins no zig for {host}"))?;
    let tarball = kept.join(file);
    let folder = kept.join(file.strip_suffix(".tar.xz").with_context(|| format!("{file} is no .tar.xz"))?);
    let program = folder.join("zig");
    // What was checked once is not fetched again: its tarball is held to its hash, and its folder is what unpacking it
    // made.
    let checked = std::fs::read(&tarball).is_ok_and(|bytes| hex(&Sha256::digest(&bytes)) == *sha256);
    if checked && program.is_file() {
        return Ok((program, tarball));
    }
    std::fs::create_dir_all(kept).with_context(|| kept.display().to_string())?;
    let bytes = fetch_zig(zig, file, sha256, project)?;
    std::fs::write(&tarball, &bytes).with_context(|| tarball.display().to_string())?;
    if folder.exists() {
        std::fs::remove_dir_all(&folder).with_context(|| folder.display().to_string())?;
    }
    let xz = lzma_rust2::XzReader::new(std::io::Cursor::new(bytes), true);
    tar::Archive::new(xz).unpack(kept).with_context(|| format!("{file} could not be unpacked"))?;
    ensure!(program.is_file(), "{file} holds no {}", program.display());
    Ok((program, tarball))
}

/// Zig's tarball, from a community mirror in random order and from ziglang.org last, each held to its signature and
/// its hash; what each that failed answered, where none served it.
fn fetch_zig(zig: &Zig, file: &str, sha256: &str, project: &Path) -> Result<Vec<u8>> {
    let cached = project.join("zig-mirrors.txt");
    // The list is read where it is published, its copy refreshed, and the copy read where it cannot be.
    let listed = match get(&zig.mirrors) {
        Ok(bytes) if !bytes.is_empty() => {
            let _ = std::fs::write(&cached, &bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => std::fs::read_to_string(&cached).with_context(|| cached.display().to_string())?,
    };
    let mut mirrors: Vec<String> = listed.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    shuffle(&mut mirrors, seed());
    let official = format!("https://ziglang.org/download/{}", zig.version);
    let mut failed = Vec::new();
    for base in mirrors.iter().chain(std::iter::once(&official)) {
        let url = format!("{base}/{file}?source={}", zig.source);
        let signature = format!("{base}/{file}.minisig?source={}", zig.source);
        let got = get(&url).and_then(|bytes| {
            let signature = String::from_utf8(get(&signature)?).context("the signature is no text")?;
            check(&zig.public_key, file, sha256, &bytes, &signature)?;
            Ok(bytes)
        });
        match got {
            Ok(bytes) => {
                eprintln!("zig {}: {file} from {base}", zig.version);
                return Ok(bytes);
            }
            Err(e) => failed.push(format!("{base}: {e:#}")),
        }
    }
    bail!("no mirror served {file} as signed and pinned:\n  {}", failed.join("\n  "))
}

/// Whether a tarball is the one asked for: signed with Zig's key, signed as this file - a signature of another, older
/// one, served in its place, is refused - and the bytes pinned.
fn check(public_key: &str, file: &str, sha256: &str, bytes: &[u8], signature: &str) -> Result<()> {
    let key = minisign_verify::PublicKey::from_base64(public_key).map_err(|e| anyhow!("Zig's key: {e}"))?;
    let signature = minisign_verify::Signature::decode(signature).map_err(|e| anyhow!("the signature: {e}"))?;
    key.verify(bytes, &signature, false).map_err(|e| anyhow!("its signature does not hold: {e}"))?;
    let signed = signed_file(signature.trusted_comment()).context("its signature names no file")?;
    ensure!(signed == file, "it is signed as {signed}, not {file}");
    let found = hex(&Sha256::digest(bytes));
    ensure!(found == sha256, "its SHA-256 is {found}, not the {sha256} pinned");
    Ok(())
}

/// The file a signature's trusted comment says it was made for (`file:<name>`, among fields parted by tabs).
fn signed_file(comment: &str) -> Option<&str> {
    comment.split('\t').find_map(|field| field.strip_prefix("file:"))
}

fn get(url: &str) -> Result<Vec<u8>> {
    ensure!(url.starts_with("https://"), "{url}: only HTTPS is fetched");
    let response = ureq::get(url).call().map_err(|e| anyhow!("{e}"))?;
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Lines in an order of `seed`'s (Fisher-Yates, by xorshift).
fn shuffle(lines: &mut [String], seed: u64) {
    let mut state = seed | 1;
    for i in (1..lines.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = usize::try_from(state % (i as u64 + 1)).unwrap_or(0);
        lines.swap(i, j);
    }
}

/// A seed of its moment and its process: a mirror chosen differently each run.
fn seed() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    // The low bits of the moment, which are what changes from one run to the next.
    let nanos = u64::try_from(now.as_nanos() & u128::from(u64::MAX)).unwrap_or(0);
    nanos ^ (u64::from(std::process::id()) << 32)
}

/// A package's project built for one platform, dated `date` (`commit_time`): the folder it was installed into.
fn build(
    zig: &Path,
    project: &Path,
    work: &Path,
    date: &str,
    package: &str,
    platform: &str,
    triple: &str,
) -> Result<PathBuf> {
    let out = work.join("build").join(package).join(platform);
    if out.exists() {
        std::fs::remove_dir_all(&out).with_context(|| out.display().to_string())?;
    }
    eprintln!("building {package} for {platform} ({triple})");
    let status = Command::new(zig)
        .current_dir(project)
        .arg("build")
        .arg(format!("-Dtarget={triple}"))
        // Small and without debug information: what ships, and what builds the same twice (Windows' debug information
        // names a temporary file of a random name).
        .arg("-Doptimize=ReleaseSmall")
        .arg("--prefix")
        .arg(&out)
        // Zig's global cache, where the release's sources are read from: it keeps every package fetched. Its local
        // cache stays the project's own (`<project>/.zig-cache`, in the copy), so that what is generated there is
        // compiled by a path relative to the project, as the sources are, and no path of this machine's reaches what
        // is built.
        .env("ZIG_GLOBAL_CACHE_DIR", work.join("global"))
        // The date a source reads (`__DATE__`, `__TIME__`: SVT-AV1 prints its build's) is the commit's, which clang
        // takes from here: the same build of a commit, whenever it is made.
        .env("SOURCE_DATE_EPOCH", date)
        .status()
        .with_context(|| format!("{} does not run", zig.display()))?;
    ensure!(status.success(), "{package} does not build for {platform}");
    names_no_build_path(&out, work)?;
    Ok(out)
}

/// What is built names no place it was built in: no folder of zig's cache, named by a hash zig makes of where the
/// files a step reads are (a generated source's path, which an assertion's message carries), and no path of the
/// folders the packages are built in - either would make what anyone else builds differ from what is released.
fn names_no_build_path(out: &Path, work: &Path) -> Result<()> {
    let work = std::fs::canonicalize(work).with_context(|| work.display().to_string())?;
    let needles = [".zig-cache/".to_owned(), work.to_string_lossy().into_owned()];
    let mut files = Vec::new();
    crate::gather(out, "", &mut files)?;
    for (name, bytes, _) in &files {
        for needle in &needles {
            let found = bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
            ensure!(!found, "{}{name} names {needle}, a place it was built in", out.display());
        }
    }
    Ok(())
}

/// When the last commit changing `packages/` was made, in seconds since 1970, as `SOURCE_DATE_EPOCH` gives it: what the
/// packages are built from, and not what is committed beside them - the extensions' manifests holding the archives'
/// hashes, which would otherwise change the date, and the archives, with each commit of them.
fn commit_time(packages: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(packages)
        .args(["log", "-1", "--format=%ct", "--", "."])
        .output()
        .context("git does not run")?;
    ensure!(output.status.success(), "git cannot say when the commit was made");
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The preprocessor built held to clang, the one zig carries: each test file in `packages/aro-cpp/tests`, preprocessed by
/// both with the options `pe2.psx` gives, has to come out as the same C tokens - however each spaces them - so that a
/// zig whose aro preprocesses differently is caught before what it built ships.
fn check_cpp(zig: &Path, cpp: &Path, tests: &Path, scratch: &Path) -> Result<()> {
    std::fs::create_dir_all(scratch).with_context(|| scratch.display().to_string())?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(tests)
        .with_context(|| tests.display().to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "c"))
        .collect();
    files.sort();
    ensure!(!files.is_empty(), "{} holds no test", tests.display());
    for file in files {
        let name = file.file_name().context("a test has a name")?.to_string_lossy().into_owned();
        let mut options: Vec<std::ffi::OsString> =
            ["-E", "-P", "-undef", "-nostdinc", "-DVER_USA", "-DUSE_INCLUDE_ASM", "-iquote"].map(Into::into).to_vec();
        options.extend([tests.join("quoted").into(), "-I".into(), tests.join("include").into()]);
        let ours = scratch.join(format!("{name}.cpp.i"));
        let theirs = scratch.join(format!("{name}.clang.i"));
        let status = Command::new(cpp).args(&options).arg("-o").arg(&ours).arg(&file).status();
        ensure!(status.is_ok_and(|s| s.success()), "{}: the preprocessor built fails on {name}", cpp.display());
        let status = Command::new(zig)
            .args(["cc", "-x", "c", "-target", "mipsel-freestanding-none"])
            .args(&options)
            .arg("-o")
            .arg(&theirs)
            .arg(&file)
            .status();
        ensure!(status.is_ok_and(|s| s.success()), "clang fails on {name}");
        let ours = std::fs::read_to_string(&ours)?;
        let theirs = std::fs::read_to_string(&theirs)?;
        let (a, b) = (c_tokens(&ours), c_tokens(&theirs));
        if let Some(at) =
            a.iter().zip(&b).position(|(x, y)| x != y).or((a.len() != b.len()).then(|| a.len().min(b.len())))
        {
            let around = |t: &[&str]| t[at.saturating_sub(5)..(at + 5).min(t.len())].join(" ");
            bail!(
                "{name}: the preprocessor built and clang part at token {at}:\n  ours:  {}\n  clang: {}",
                around(&a),
                around(&b)
            );
        }
    }
    Ok(())
}

/// C's tokens in preprocessed text, its line markers aside.
fn c_tokens(text: &str) -> Vec<&str> {
    const PUNCTUATORS: [&str; 23] = [
        "<<=", ">>=", "...", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=", "-=", "*=", "/=",
        "%=", "&=", "|=", "^=", "##",
    ];
    let mut tokens = Vec::new();
    for line in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let start = i;
            let c = b[i];
            if c.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            if c == b'"' || c == b'\'' {
                i += 1;
                while i < b.len() && b[i] != c {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
            } else if c.is_ascii_alphabetic() || c == b'_' {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
            } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
                i += 1;
                while i < b.len() {
                    if matches!(b[i], b'e' | b'E' | b'p' | b'P') && matches!(b.get(i + 1), Some(b'+' | b'-')) {
                        i += 2;
                    } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.' {
                        i += 1;
                    } else {
                        break;
                    }
                }
            } else {
                i += PUNCTUATORS.iter().find(|p| line[i..].starts_with(*p)).map_or(1, |p| p.len());
            }
            tokens.push(&line[start..i]);
        }
    }
    tokens
}

/// A package built for a platform, archived, with the license files of every component linked into it there
/// (`licenses/<component>/<file>`).
fn pack(package: &str, archive: &Archive, used: &[Used]) -> Result<()> {
    let mut files = Vec::new();
    crate::gather(&archive.built, "", &mut files)?;
    let mut files: Vec<(String, Vec<u8>, u32)> =
        files.into_iter().map(|(path, bytes, mode)| (path.trim_start_matches('/').to_string(), bytes, mode)).collect();
    for used in used.iter().filter(|u| u.component.linked_into(package, &archive.platform)) {
        for file in &used.component.license_files {
            let path = used.unpacked.join(file);
            let bytes = std::fs::read(&path)
                .with_context(|| format!("{}'s license: {}", used.component.name, path.display()))?;
            files.push((format!("licenses/{}/{file}", used.component.name), bytes, 0o644));
        }
    }
    write_zip(files, &archive.path)
}

/// Files written as a zip, entries in their order of name: the same files make the same bytes.
fn write_zip(mut files: Vec<(String, Vec<u8>, u32)>, to: &Path) -> Result<()> {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let entries: Vec<codecs::zip::Entry> =
        files.iter().map(|(name, data, mode)| codecs::zip::Entry { name, data, deflate: true, mode: *mode }).collect();
    std::fs::write(to, codecs::zip::write(&entries)).with_context(|| to.display().to_string())
}

/// `packages/components.toml`.
fn read_components(path: &Path) -> Result<Vec<Component>> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let table: toml::Table = text.parse().with_context(|| path.display().to_string())?;
    let entries =
        table.get("component").and_then(toml::Value::as_array).context("components.toml: no [[component]]")?;
    let strings = |entry: &toml::Table, key: &str| -> Option<Result<Vec<String>>> {
        entry.get(key).map(|v| {
            let list = v.as_array().with_context(|| format!("`{key}` is no list"))?;
            list.iter()
                .map(|x| x.as_str().map(str::to_string).with_context(|| format!("`{key}` holds no text")))
                .collect()
        })
    };
    let mut components = Vec::new();
    for entry in entries {
        let entry = entry.as_table().context("a [[component]] is no table")?;
        let text = |key: &str| -> Result<String> {
            Ok(entry
                .get(key)
                .and_then(toml::Value::as_str)
                .with_context(|| format!("a component has no `{key}`"))?
                .to_string())
        };
        let zig = entry.get("zig").and_then(toml::Value::as_bool).unwrap_or(false);
        let component = Component {
            url: if zig { None } else { Some(text("url")?) },
            name: text("name")?,
            version: text("version")?,
            license: text("license")?,
            license_files: strings(entry, "license_files").transpose()?.unwrap_or_default(),
            upstream: text("upstream")?,
            linked: match text("role")?.as_str() {
                "linked" => true,
                "tool" => false,
                other => bail!("a component's role is linked or tool, not {other}"),
            },
            platforms: strings(entry, "platforms").transpose()?,
            packages: strings(entry, "packages").transpose()?,
        };
        if let Some(url) = &component.url {
            ensure!(
                names_version(url, &component.version),
                "{url} does not name {}'s version, {}",
                component.name,
                component.version
            );
        }
        components.push(component);
    }
    Ok(components)
}

/// Whether a URL names a version, as written or with underscores for its dots (`R_2_9_0`).
fn names_version(url: &str, version: &str) -> bool {
    url.contains(version) || url.contains(&version.replace('.', "_"))
}

/// Each `.url` a manifest declares with the `.hash` after it.
fn declared(manifest: &str) -> Vec<(String, String)> {
    let value = |line: &str, key: &str| {
        let rest = line.trim().strip_prefix(key)?.trim_start().strip_prefix('=')?.trim_start().strip_prefix('"')?;
        Some(rest[..rest.find('"')?].to_string())
    };
    let mut pairs = Vec::new();
    let mut url = None;
    for line in manifest.lines() {
        if let Some(u) = value(line, ".url") {
            url = Some(u);
        } else if let (Some(h), Some(u)) = (value(line, ".hash"), url.take()) {
            pairs.push((u, h));
        }
    }
    pairs
}

/// What zig makes in a project's folder: what it fetched for the project, its cache, and its default prefix.
const MADE: [&str; 3] = ["zig-pkg", ".zig-cache", "zig-out"];

/// `from` (`packages/`) copied to `to`, where the packages are built. zig fetches a project's dependencies into its
/// folder and keeps its cache there, and compiles every source by a path relative to the folder - a path that reaches
/// what is built, as an assertion's file name - so the copy keeps `packages/` free of what zig makes while what is
/// built names its sources as it would there. The copy is kept from one run to the next, zig's cache in it with it,
/// and made to hold what `from` does: a file changed is written again, one gone removed, what zig made left.
fn mirror(from: &Path, to: &Path) -> Result<()> {
    fn copy(from: &Path, to: &Path, rel: &Path, wanted: &mut std::collections::BTreeSet<PathBuf>) -> Result<()> {
        let folder = from.join(rel);
        for entry in std::fs::read_dir(&folder).with_context(|| folder.display().to_string())? {
            let entry = entry?;
            if MADE.iter().any(|m| entry.file_name() == *m) {
                continue;
            }
            let rel = rel.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy(from, to, &rel, wanted)?;
                continue;
            }
            let (source, dest) = (from.join(&rel), to.join(&rel));
            let bytes = std::fs::read(&source).with_context(|| source.display().to_string())?;
            if std::fs::read(&dest).ok().as_deref() != Some(&bytes[..]) {
                let parent = dest.parent().context("a file is in a folder")?;
                std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
                std::fs::write(&dest, &bytes).with_context(|| dest.display().to_string())?;
            }
            let permissions = std::fs::metadata(&source).with_context(|| source.display().to_string())?.permissions();
            std::fs::set_permissions(&dest, permissions).with_context(|| dest.display().to_string())?;
            wanted.insert(rel);
        }
        Ok(())
    }
    fn prune(to: &Path, rel: &Path, wanted: &std::collections::BTreeSet<PathBuf>) -> Result<()> {
        let folder = to.join(rel);
        for entry in std::fs::read_dir(&folder).with_context(|| folder.display().to_string())? {
            let entry = entry?;
            if MADE.iter().any(|m| entry.file_name() == *m) {
                continue;
            }
            let rel = rel.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                prune(to, &rel, wanted)?;
                if std::fs::read_dir(to.join(&rel))?.next().is_none() {
                    std::fs::remove_dir(to.join(&rel)).with_context(|| rel.display().to_string())?;
                }
            } else if !wanted.contains(&rel) {
                std::fs::remove_file(to.join(&rel)).with_context(|| rel.display().to_string())?;
            }
        }
        Ok(())
    }
    let mut wanted = std::collections::BTreeSet::new();
    copy(from, to, Path::new(""), &mut wanted)?;
    prune(to, Path::new(""), &wanted)
}

/// What a package's project fetched, each folder of its `zig-pkg/` the component its URL is - the URL a manifest
/// declares with the folder's hash, among the projects of `packages/` and the packages fetched - and the zig that built
/// it.
fn resolve(components: &[Component], packages: &Path, project: &Path, zig: &Path) -> Result<Vec<Used>> {
    let fetched = project.join("zig-pkg");
    let mut manifests: Vec<PathBuf> = vec![project.join("build.zig.zon")];
    for folder in [packages.join("ports"), fetched.clone()] {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            manifests.push(entry.path().join("build.zig.zon"));
        }
    }
    let mut urls: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for manifest in manifests.iter().filter(|m| m.is_file()) {
        let text = std::fs::read_to_string(manifest).with_context(|| manifest.display().to_string())?;
        for (url, hash) in declared(&text) {
            urls.entry(hash).or_default().push(url);
        }
    }
    let zig_component = components.iter().find(|c| c.url.is_none()).context("components.toml describes no zig")?;
    let mut used = vec![Used { component: zig_component.clone(), unpacked: zig.to_path_buf(), hash: None }];
    let mut hashes: Vec<String> = std::fs::read_dir(&fetched)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    hashes.sort();
    for hash in hashes {
        let named = urls.get(&hash).with_context(|| format!("{}: no manifest declares {hash}", project.display()))?;
        let component = components
            .iter()
            .find(|c| c.url.as_ref().is_some_and(|u| named.contains(u)))
            .with_context(|| format!("components.toml describes no {} ({hash})", named.join(", ")))?;
        used.push(Used { component: component.clone(), unpacked: fetched.join(&hash), hash: Some(hash) });
    }
    Ok(used)
}

/// The sources of a release: each component's as zig's cache keeps it, named by its name and version; the
/// `packages/` folder that builds them; and the zig it was built with, where that is the pinned one.
fn sources(packages: &Path, global: &Path, zig: Option<&Path>, used: &[Used], to: &Path) -> Result<()> {
    for used in used {
        let Some(hash) = &used.hash else { continue };
        let kept = global.join("p").join(format!("{hash}.tar.gz"));
        ensure!(kept.is_file(), "zig fetched {hash}, and its cache keeps no {}", kept.display());
        let name = format!("{}-{}.tar.gz", used.component.name, used.component.version);
        std::fs::copy(&kept, to.join(name)).with_context(|| kept.display().to_string())?;
    }
    // The folder as it is: what builds the packages and what made their tables, without what zig makes in it.
    let mut files = Vec::new();
    crate::gather(packages, "packages", &mut files)?;
    files.retain(|(name, _, _)| !name.split('/').any(|part| MADE.contains(&part)));
    write_zip(files, &to.join("pe2-editor-packages.zip"))?;
    if let Some(zig) = zig {
        let name = zig.file_name().context("zig's tarball has a name")?;
        std::fs::copy(zig, to.join(name)).with_context(|| zig.display().to_string())?;
    }
    Ok(())
}

/// What a release is, for its page.
fn notes(config: &Config, name: &str, tag: &str, built: &[Archive], used: &[Used], zig: Option<&str>) -> String {
    let package = &config.packages[name];
    let mut out = String::new();
    let _ = writeln!(out, "# {tag}\n");
    let _ = writeln!(out, "{}: {}.\n", package.name, package.description);
    if zig.is_none() {
        let _ = writeln!(out, "**Built with a zig other than the pinned one: not for publishing.**\n");
    }
    let _ = writeln!(
        out,
        "License: {}. Copyright: {}. Upstream: {}.\n",
        package.license, package.copyright, package.upstream
    );
    let _ = writeln!(out, "## Files\n");
    for archive in built {
        if let Some(file) = archive.path.file_name() {
            let _ = writeln!(out, "- `{}`", file.to_string_lossy());
        }
    }
    let _ = writeln!(
        out,
        "\n## What it is made of\n\nEach archive holds, in `licenses/<component>/`, the license of every component \
         linked into it on its platform. `sources/` holds each component's source as zig keeps it, named as below \
         (zig's hash is its name in zig's cache), the `packages/` folder that builds it (`pe2-editor-packages.zip`) \
         and the zig it was built with ({}).\n",
        config.zig.version
    );
    let _ = writeln!(out, "| Component | Version | License | Role | Source | Upstream | zig hash |");
    let _ = writeln!(out, "|---|---|---|---|---|---|---|");
    for used in used {
        let c = &used.component;
        let role = match (&c.platforms, c.linked) {
            (_, false) => "used to build".to_string(),
            (None, true) => "linked".to_string(),
            (Some(p), true) => format!("linked on {}", p.join(", ")),
        };
        let (source, hash) = match &used.hash {
            Some(hash) => (format!("{}-{}.tar.gz", c.name, c.version), format!("`{hash}`")),
            None => (zig.unwrap_or("(not the pinned zig)").to_string(), String::new()),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {role} | `{source}` | {} | {hash} |",
            c.name, c.version, c.license, c.upstream
        );
    }
    let _ = writeln!(
        out,
        "\n## Building it again\n\n`cargo xtask packages --package {name}` in the editor's repository, at the commit this \
         release was built from, makes the same files: `SHA256SUMS` says what they are."
    );
    out
}

/// The `[[setup]]` entry of each archive: where the release will serve it, its SHA-256, and each program an extension
/// takes of it, by the hash `pe2_setup` holds it to.
fn setup(config: &Config, name: &str, tag: &str, built: &[Archive]) -> Result<String> {
    let package = &config.packages[name];
    let mut out =
        String::from("# The `[[setup]]` entries of this release's archives, for the extensions' manifests.\n");
    for archive in built {
        let file = archive.path.file_name().context("an archive has a name")?.to_string_lossy();
        let bytes = std::fs::read(&archive.path).with_context(|| archive.path.display().to_string())?;
        let mut take = Vec::new();
        for (program, to) in &package.take {
            // A program is `.exe` on Windows; a folder is what it is.
            let windows = archive.platform.starts_with("windows");
            let exe = if windows && archive.built.join(format!("{program}.exe")).is_file() { ".exe" } else { "" };
            let (program, to) = (format!("{program}{exe}"), format!("{to}{exe}"));
            let sha256 = pe2_setup::hash(&archive.built.join(&program))?;
            take.push(format!("{program:?} = {{ to = {to:?}, sha256 = {sha256:?} }}"));
        }
        let _ = write!(
            out,
            "\n[[setup]]\nname = {:?}\nlicense = {:?}\ncopyright = {:?}\nplatform = {:?}\nurl = \"{}/releases/download/{tag}/{file}\"\nsha256 = {:?}\ntake = {{ {} }}\n",
            package.name,
            package.license,
            package.copyright,
            archive.platform,
            config.repository,
            hex(&Sha256::digest(&bytes)),
            take.join(", "),
        );
    }
    Ok(out)
}

/// Every file of the release with its SHA-256, as `sha256sum` writes them, by its path within it.
fn sums(release: &Path) -> Result<String> {
    let mut files = Vec::new();
    crate::gather(release, "", &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (name, bytes, _) in files {
        let name = name.trim_start_matches('/');
        if name != "SHA256SUMS" {
            let _ = writeln!(out, "{}  {name}", hex(&Sha256::digest(&bytes)));
        }
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

#[cfg(test)]
mod tests {
    use super::{Component, c_tokens, check, declared, mirror, names_version, shuffle, signed_file};

    #[test]
    fn tokens_are_the_same_however_spaced() {
        assert_eq!(c_tokens("a=(b +0x10)->c;"), c_tokens("# 1 \"x.c\"\na = ( b + 0x10 ) -> c ;"));
        assert_eq!(c_tokens("x <<= 1e+5 \"a \\\" b\" '\\''"), ["x", "<<=", "1e+5", "\"a \\\" b\"", "'\\''"]);
        assert_ne!(c_tokens("a+++b"), c_tokens("a + ++b"));
    }

    /// Zig 0.17.1's signature of its Linux x86_64 tarball, as its mirrors serve it.
    const SIGNATURE: &str = "untrusted comment: signature from minisign secret key\n\
        RUSGOq2NVecA2R8ojCoURkRiXJxI84Hv4ISwRsDzdh61qdlHzveXv/ZMCk8rS0Gy4HOBoktvgvfbACTooKMQfpBFnWuTrhKkHgc=\n\
        trusted comment: timestamp:1793729090\tfile:zig-x86_64-linux-0.17.1.tar.xz\thashed\n\
        Y+QP52U3hd6UXp3NPwsgtO8I2VVRduHwIICDBVCgpZ4MVmYsQMrwsVnV8IVFoAcXGiJOTBSAForDAcziQHqxDg==\n";
    const KEY: &str = "RWSGOq2NVecA2UPNdBUZykf1CCb147pkmdtYxgb3Ti+JO/wCYvhbAb/U";

    #[test]
    fn a_signature_names_the_file_it_was_made_for() {
        let signature = minisign_verify::Signature::decode(SIGNATURE).unwrap();
        assert_eq!(signed_file(signature.trusted_comment()), Some("zig-x86_64-linux-0.17.1.tar.xz"));
        assert_eq!(signed_file("timestamp:1"), None);
    }

    #[test]
    fn what_the_signature_was_not_made_of_is_refused() {
        let refused = check(KEY, "zig-x86_64-linux-0.17.1.tar.xz", "", b"not zig", SIGNATURE).unwrap_err();
        assert!(format!("{refused:#}").contains("signature does not hold"), "{refused:#}");
    }

    #[test]
    fn a_signature_under_another_key_is_refused() {
        // Zig's key with its key id changed: a key of someone else's.
        let other = "RWSGOq2NVecA2UPNdBUZykf1CCb147pkmdtYxgb3Ti+JO/wCYvhbAb/A";
        assert!(check(other, "zig-x86_64-linux-0.17.1.tar.xz", "", b"not zig", SIGNATURE).is_err());
    }

    #[test]
    fn a_manifest_declares_each_url_with_its_hash() {
        let manifest = r#".{
    .name = .x264,
    .dependencies = .{
        .harvested = .{ .path = "../harvested" },
        .x264 = .{
            .url = "git+https://code.videolan.org/videolan/x264.git#b35605ace3ddf7c1a5d67a2eb553f034aef41d55",
            .hash = "N-V-__8AAEIoWgCLQEWKw2Po2kFH0XPTsoriq2HXqBgB7sgg",
        },
        .nasm = .{ .url = "git+https://github.com/allyourcodebase/nasm#f11aa6e", .hash = "nasm-3.0.1-B_Nn" },
    },
}"#;
        let pairs = declared(manifest);
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        assert_eq!(pairs[0].1, "N-V-__8AAEIoWgCLQEWKw2Po2kFH0XPTsoriq2HXqBgB7sgg");
        assert!(pairs[0].0.ends_with("#b35605ace3ddf7c1a5d67a2eb553f034aef41d55"));
    }

    #[test]
    fn a_url_names_its_version_as_written_or_with_underscores() {
        assert!(names_version("https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz", "9.0.2"));
        assert!(names_version("git+https://github.com/libexpat/libexpat.git?ref=R_2_9_0#a76b117", "2.9.0"));
        assert!(names_version("git+https://code.videolan.org/videolan/x264.git#b35605ace3dd", "b35605a"));
        assert!(!names_version("https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz", "9.0.3"));
    }

    #[test]
    fn a_component_is_linked_where_it_says() {
        let libva = Component {
            url: Some("https://example/libva-2.24.1.tar.gz".into()),
            name: "libva".into(),
            version: "2.24.1".into(),
            license: "MIT".into(),
            license_files: vec!["COPYING".into()],
            upstream: String::new(),
            linked: true,
            platforms: Some(vec!["linux-x86_64".into()]),
            packages: None,
        };
        assert!(libva.linked_into("ffmpeg", "linux-x86_64"));
        assert!(!libva.linked_into("ffmpeg", "windows-x86_64"));
        let zig = Component { url: None, platforms: None, packages: Some(vec!["aro-cpp".into()]), ..libva.clone() };
        assert!(zig.linked_into("aro-cpp", "macos-aarch64"));
        assert!(!zig.linked_into("python", "macos-aarch64"));
        let tool = Component { linked: false, platforms: None, ..libva };
        assert!(!tool.linked_into("ffmpeg", "linux-x86_64"));
    }

    #[test]
    fn mirrors_are_shuffled_not_lost() {
        let mut lines: Vec<String> = (0..17).map(|i| format!("https://mirror{i}.example")).collect();
        let before = lines.clone();
        shuffle(&mut lines, 0x1234_5678_9abc_def0);
        assert_ne!(lines, before);
        lines.sort();
        let mut sorted = before;
        sorted.sort();
        assert_eq!(lines, sorted);
    }

    #[test]
    fn the_copy_holds_what_packages_holds_and_what_zig_made() {
        let root = std::env::temp_dir().join(format!("xtask-mirror-{}", std::process::id()));
        let (from, to) = (root.join("packages"), root.join("projects"));
        let write = |path: std::path::PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(from.join("ffmpeg/build.zig"), "new");
        write(from.join("ports/x264/build.zig.zon"), "kept");
        write(from.join("ffmpeg/zig-pkg/fetched"), "never copied");
        write(to.join("ffmpeg/build.zig"), "old");
        write(to.join("gone/build.zig"), "removed");
        write(to.join("ffmpeg/.zig-cache/o/object"), "left");
        mirror(&from, &to).unwrap();
        let read = |path: &str| std::fs::read_to_string(to.join(path)).ok();
        assert_eq!(read("ffmpeg/build.zig").as_deref(), Some("new"));
        assert_eq!(read("ports/x264/build.zig.zon").as_deref(), Some("kept"));
        assert_eq!(read("ffmpeg/.zig-cache/o/object").as_deref(), Some("left"));
        assert!(!to.join("gone").exists());
        assert!(!to.join("ffmpeg/zig-pkg").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
