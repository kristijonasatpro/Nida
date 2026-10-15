#!/usr/bin/env python3
"""Make a Mac app carry every library its programs load, and say whose.

For each program in Contents/MacOS (nida, and the ffmpeg it runs for REC and
STREAM) and each library those pull in, every load command that points
outside the system (Homebrew's /opt/homebrew, /usr/local) is followed, as
dyld would follow it from where the program was built: the library is copied
into Contents/Frameworks under the name the load command used, and the
command is rewritten to @rpath/<name>. The programs get
@executable_path/../Frameworks as their run path; the libraries lose the run
paths they were built with, so nothing is looked for outside the app. Then
everything is signed again, ad hoc and inside out, since rewriting a load
command breaks the signature it had.

Then it checks that no load command or run path still leaves the app and
that nothing needs a newer macOS than --min-macos, and writes
Contents/Resources/licenses: each Homebrew package's licence files and
THIRD-PARTY.md (version, licence, source); with --cargo-args, the Rust crates
built into nida and every licence or notice file in them (RUST-CRATES.md);
the credits the bundled shaders give (SHADERS.md).

With --sources, it also makes the archive the GPL asks to be offered beside
the DMG: the exact source Homebrew built each bundled package from (patches
and resources too), the formula that built it, and its install receipt.

Usage: bundle-macos-libs.py APP --min-macos 15.0 [--identifier ID]
         [--origin nida=target/release/varda] [--origin ffmpeg=PATH]
         [--cargo-args "--no-default-features --features screen-capture"]
         [--version 0.9.0 --sources Nida-0.9.0-third-party-source.tar.gz]
"""

import argparse
import hashlib
import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

SYSTEM = ("/usr/lib/", "/System/")
OWN_RPATH = "@executable_path/../Frameworks"
REPO = "https://github.com/kristijonasatpro/Nida"
KEG_LICENCE = re.compile(r"^(licen[cs]e|copying|copyright|notice|patents|unlicense)([._-].*)?$", re.I)
# Anywhere in a crate: licence and notice files, whatever their suffix.
NOTICE = re.compile(r"licen[cs]e|copying|copyright|notice|patents|^ofl\.|^ufl\.", re.I)
NOT_NOTICES = {".rs", ".c", ".cc", ".cpp", ".h", ".hpp", ".py", ".js", ".ts", ".json", ".toml",
               ".yml", ".yaml", ".sh", ".go", ".java", ".m", ".mm", ".s", ".asm", ".cmake",
               ".in", ".png", ".jpg", ".svg", ".ttf", ".otf", ".wgsl", ".glsl", ".frag", ".vert"}
# Notices a name does not give away: the Hack font's, beside the font.
EXTRA_NOTICES = {"epaint_default_fonts": ["fonts/Hack-Regular.txt"]}


def run(*args, env=None):
    result = subprocess.run(args, capture_output=True, text=True, env=env)
    if result.returncode != 0:
        sys.exit(f"failed: {' '.join(map(str, args))}\n{result.stderr}{result.stdout[-2000:]}")
    return result.stdout


def is_macho(path):
    try:
        with open(path, "rb") as f:
            magic = f.read(4)
    except OSError:
        return False
    return magic in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca")


def load_commands(path):
    """The libraries `path` loads, and its own install name (None for a program)."""
    own = run("otool", "-D", path).splitlines()[1:]
    own = own[0].strip() if own else None
    refs = []
    for line in run("otool", "-L", path).splitlines()[1:]:
        ref = line.strip().split(" (compatibility")[0].strip()
        if ref and ref != own:
            refs.append(ref)
    return refs, own


def macho_info(path):
    """Run paths and the oldest macOS `path` runs on, from its load commands."""
    rpaths, minos, cmd = [], None, None
    for line in run("otool", "-l", path).splitlines():
        line = line.strip()
        if line.startswith("cmd "):
            cmd = line.split()[1]
        elif cmd == "LC_RPATH" and line.startswith("path "):
            rpaths.append(line.split()[1])
        elif cmd == "LC_BUILD_VERSION" and line.startswith("minos "):
            minos = line.split()[1]
        elif cmd == "LC_VERSION_MIN_MACOSX" and line.startswith("version "):
            minos = line.split()[1]
    return rpaths, minos


def own_rpaths(origin, program_dir):
    """`origin`'s run paths with @loader_path and @executable_path filled in."""
    here = os.path.dirname(os.path.realpath(origin))
    rpaths, _ = macho_info(origin)
    return [r.replace("@loader_path", here).replace("@executable_path", program_dir) for r in rpaths]


def resolve(ref, origin, program_dir, search):
    """The file a load command in `origin` (where it was built to live) means,
    loaded by the program in `program_dir`: an @rpath is looked for in
    `search`, `origin`'s own run paths then those of the images that loaded
    it, as dyld does."""
    def there(path):
        return os.path.realpath(path) if os.path.exists(path) else None
    if ref.startswith("@loader_path/"):
        return there(os.path.join(os.path.dirname(os.path.realpath(origin)), ref[len("@loader_path/"):]))
    if ref.startswith("@executable_path/"):
        return there(os.path.join(program_dir, ref[len("@executable_path/"):]))
    if ref.startswith("@rpath/"):
        for rpath in search:
            hit = there(os.path.join(rpath, ref[len("@rpath/"):]))
            if hit:
                return hit
        return None
    return there(ref)


def version_tuple(text):
    return tuple(int(p) for p in text.split("."))


def bundle(app, min_macos, identifier, origins):
    macos, frameworks = app / "Contents/MacOS", app / "Contents/Frameworks"
    frameworks.mkdir(parents=True, exist_ok=True)
    programs = sorted(p for p in macos.iterdir() if p.is_file() and is_macho(p))
    if not programs:
        sys.exit(f"no programs in {macos}")

    # Follow the load commands from the programs as built (their originals),
    # each library with the run paths of the chain that loaded it, copying
    # each once under the name it is first asked for by.
    named = {}  # real path -> name in Frameworks
    original = {}  # file in the app -> where it came from
    meant = {}  # (file in the app, load command) -> the real path it means
    queue = []
    for program in programs:
        origin = Path(origins.get(program.name, program))
        original[program] = origin
        queue.append((program, os.path.dirname(os.path.realpath(origin)), []))
    while queue:
        file, program_dir, inherited = queue.pop(0)
        search = own_rpaths(original[file], program_dir) + inherited
        refs, _ = load_commands(original[file])
        for ref in refs:
            if ref.startswith(SYSTEM):
                continue
            real = resolve(ref, original[file], program_dir, search)
            if real is None:
                sys.exit(f"{file.name} loads {ref}, which is not there")
            meant[(file, ref)] = real
            if real in named:
                continue
            name = os.path.basename(ref)
            if name in named.values():
                sys.exit(f"two different libraries are called {name}")
            named[real] = name
            copy = frameworks / name
            shutil.copyfile(real, copy)
            copy.chmod(0o755)
            original[copy] = Path(real)
            queue.append((copy, program_dir, search))

    # Rewrite: every outside load command to @rpath/<name>, each library's
    # own name too, and only the app's own run path left.
    for file in original:
        refs, own = load_commands(file)
        args = []
        for ref in refs:
            if not ref.startswith(SYSTEM):
                args += ["-change", ref, f"@rpath/{named[meant[(file, ref)]]}"]
        if own is not None:
            args += ["-id", f"@rpath/{file.name}"]
        rpaths, _ = macho_info(file)
        for rpath in rpaths:
            if not (file in programs and rpath == OWN_RPATH):
                args += ["-delete_rpath", rpath]
        if file in programs and OWN_RPATH not in rpaths:
            args += ["-add_rpath", OWN_RPATH]
        if args:
            run("install_name_tool", *args, str(file))

    # Signed again, inside out: libraries, the other programs, the app.
    for file in sorted(frameworks.iterdir()):
        run("codesign", "--force", "--sign", "-", str(file))
    main = macos / "nida"
    for program in programs:
        if program != main:
            run("codesign", "--force", "--sign", "-", str(program))
    run("codesign", "--force", "--sign", "-", "--identifier", identifier, str(app))

    needs = check(app, min_macos)
    # The app says the macOS it needs, so an older one refuses it plainly.
    plist = app / "Contents/Info.plist"
    with open(plist, "rb") as f:
        info = plistlib.load(f)
    if info.get("LSMinimumSystemVersion") != needs:
        info["LSMinimumSystemVersion"] = needs
        with open(plist, "wb") as f:
            plistlib.dump(info, f)
        print(f"needs macOS {needs}")
    return named


def check(app, min_macos):
    """Nothing loaded from outside the app, and nothing that needs a macOS of
    a later major version than `min_macos` (Homebrew's packages for macOS 15
    may need a 15.x). Returns the macOS the app needs: the newest any of its
    files asks for, `min_macos` at least."""
    frameworks = app / "Contents/Frameworks"
    problems = []
    needs = min_macos
    for file in sorted(p for p in app.rglob("*") if p.is_file() and is_macho(p)):
        refs, _ = load_commands(file)
        for ref in refs:
            if ref.startswith(SYSTEM):
                continue
            if not ref.startswith("@rpath/") or not (frameworks / ref[len("@rpath/"):]).exists():
                problems.append(f"{file.relative_to(app)} loads {ref}")
        rpaths, minos = macho_info(file)
        for rpath in rpaths:
            if rpath != OWN_RPATH:
                problems.append(f"{file.relative_to(app)} looks in {rpath}")
        if minos and version_tuple(minos)[0] > version_tuple(min_macos)[0]:
            problems.append(f"{file.relative_to(app)} needs macOS {minos}")
        elif minos and version_tuple(minos) > version_tuple(needs):
            needs = minos
    if problems:
        sys.exit("the app is not self-contained:\n  " + "\n  ".join(problems))
    run("codesign", "--verify", "--deep", "--strict", str(app))
    return needs


def formula_of(real):
    """(formula, version, keg) of a file Homebrew installed, from its Cellar path."""
    parts = Path(real).parts
    if "Cellar" not in parts:
        return None
    at = parts.index("Cellar")
    return parts[at + 1], parts[at + 2], Path(*parts[: at + 3])


def keg_version(f):
    """The Cellar folder name the formula `f` (brew info) installs."""
    stable, revision = f["versions"]["stable"], f.get("revision", 0)
    return f"{stable}_{revision}" if revision else stable


def homebrew_packages(named, origins):
    packages = {}
    for real in list(named) + [os.path.realpath(o) for o in origins.values()]:
        found = formula_of(os.path.realpath(real))
        if found:
            packages[found[0]] = found
    info = {}
    if packages:
        data = json.loads(run("brew", "info", "--json=v2", *sorted(packages)))
        info = {f["name"]: f for f in data["formulae"]}
    return packages, info


def spdx_ids(expression):
    """The licence ids in an SPDX expression such as "(MIT OR Apache-2.0) AND Zlib"."""
    words = re.split(r"[\s()/]+", expression or "")
    return sorted({w for w in words if w and w.upper() not in ("OR", "AND", "WITH") and w != "?"})


def spdx_texts(ids, out):
    """The standard text of each licence id, from the SPDX licence list, for
    what carries no licence file of its own."""
    out.mkdir(parents=True, exist_ok=True)
    missing = []
    for spdx in ids:
        url = f"https://raw.githubusercontent.com/spdx/license-list-data/main/text/{spdx}.txt"
        try:
            with urllib.request.urlopen(url, timeout=60) as r:
                (out / f"{spdx}.txt").write_bytes(r.read())
        except OSError:
            missing.append(spdx)
    if missing:
        sys.exit(f"no standard licence text found for: {', '.join(missing)}")


def source_url(f):
    return (f.get("urls", {}).get("stable") or {}).get("url") or f.get("homepage", "")


def licences(app, packages, info, version):
    """Each bundled Homebrew package's licence files, and THIRD-PARTY.md."""
    out = app / "Contents/Resources/licenses"
    out.mkdir(parents=True, exist_ok=True)
    rows, bare = [], set()
    for name in sorted(packages):
        _, installed, keg = packages[name]
        f = info.get(name, {})
        note = "" if not f or keg_version(f) == installed else f" (the formula is now {keg_version(f)})"
        licence = f.get("license") or "see its files"
        files = [p for p in sorted(keg.iterdir()) if p.is_file() and KEG_LICENCE.match(p.name)]
        if not files:
            licence += " (standard text in texts/)"
            bare.update(spdx_ids(f.get("license")))
        rows.append(f"| {name} | {installed}{note} | {licence} | {source_url(f)} |")
        for file in files:
            (out / name).mkdir(exist_ok=True)
            shutil.copyfile(file, out / name / file.name)
    spdx_texts(sorted(bare), out / "texts")
    (out / "THIRD-PARTY.md").write_text(
        "# Libraries inside Nida.app\n\n"
        "Copied from Homebrew when this app was built. Each one's licence files are\n"
        "in the folder of its name beside this file. The exact source of each,\n"
        "with the Homebrew formula that built it, is in\n"
        f"Nida-{version}-third-party-source.tar.gz on the release page this app came\n"
        f"from ({REPO}/releases); the address below is where its makers publish it.\n\n"
        "| Package | Version | Licence | Source |\n|---|---|---|---|\n" + "\n".join(rows) + "\n"
    )


def notice_files(root, declared):
    """Licence and notice files anywhere in a crate (any file in a licences
    folder too), and the ones it declares."""
    found = set()
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in (".git", "target", "node_modules")]
        in_licences = any(NOTICE.search(part) for part in Path(dirpath).relative_to(root).parts)
        for name in filenames:
            path = Path(dirpath) / name
            if (NOTICE.search(name) or in_licences) and path.suffix.lower() not in NOT_NOTICES \
                    and path.is_file() and path.stat().st_size < 512_000:
                found.add(path)
    for extra in declared:
        path = root / extra
        if path.is_file():
            found.add(path)
    return sorted(found)


def rust_licences(app, cargo_args):
    """The crates built into nida for `cargo_args`, with their notice files."""
    out = app / "Contents/Resources/licenses/rust"
    out.mkdir(parents=True, exist_ok=True)
    tree = run(
        "cargo", "tree", "--locked", "-e", "normal", "--prefix", "none",
        "--target", "aarch64-apple-darwin", "--format", "{p}|{l}|{r}", *cargo_args.split(),
    )
    meta = json.loads(run("cargo", "metadata", "--locked", "--format-version", "1"))
    root = meta["resolve"]["root"]
    crates = {(p["name"], p["version"]): p for p in meta["packages"] if p["id"] != root}
    rows, bare, seen = [], [], set()
    for line in tree.splitlines():
        line = line.replace(" (*)", "").replace(" (proc-macro)", "").strip()
        if "|" not in line:
            continue
        package, licence, repository = line.split("|", 2)
        name, version = package.split(" ")[:2]
        version = version.lstrip("v")
        if (name, version) in seen or (name, version) not in crates:
            continue
        seen.add((name, version))
        crate = crates[(name, version)]
        rows.append(f"| {name} | {version} | {licence or 'see its files'} | {repository} |")
        root_dir = Path(crate["manifest_path"]).parent
        declared = EXTRA_NOTICES.get(name, []) + ([crate["license_file"]] if crate.get("license_file") else [])
        files = notice_files(root_dir, declared)
        if not files:
            authors = ", ".join(crate.get("authors") or []) or "its authors"
            bare.append(f"| {name} | {version} | {licence or '?'} | {authors} |")
        for file in files:
            copied = out / f"{name}-{version}" / file.relative_to(root_dir)
            copied.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(file, copied)
    text = (
        "# Rust crates built into Nida\n\n"
        "Every licence or notice file each one carries is in rust/<name>-<version>\n"
        "beside this file, at the place it has in the crate. Their source is on\n"
        "crates.io, at the versions Nida's Cargo.lock names.\n\n"
        "| Crate | Version | Licence | Source |\n|---|---|---|---|\n" + "\n".join(sorted(rows)) + "\n"
    )
    if bare:
        text += (
            "\n## Crates without a licence file\n\n"
            "These give their licence only in their manifest; the copyright is their\n"
            "authors', and the licence's standard text is in rust/texts.\n\n"
            "| Crate | Version | Licence | Authors |\n|---|---|---|---|\n"
            + "\n".join(sorted(bare)) + "\n"
        )
        spdx_texts(sorted({i for b in bare for i in spdx_ids(b.split(" | ")[2])}), out / "texts")
        print(f"{len(bare)} crates carry no licence file: " + ", ".join(b.split(" | ")[0][2:] for b in sorted(bare)))
    (out.parent / "RUST-CRATES.md").write_text(text)
    return len(rows)


def shader_credits(app):
    """SHADERS.md: what each bundled shader credits, as its file says."""
    shaders = app / "Contents/Resources/shaders"
    credit = re.compile(r'"CREDIT"\s*:\s*"((?:[^"\\]|\\.)*)"')
    rows = []
    for file in sorted(shaders.rglob("*")):
        if file.is_file() and file.suffix in (".fs", ".comp", ".vs"):
            m = credit.search(file.read_text(errors="replace")[:4000])
            if m:
                rows.append(f"| {file.relative_to(shaders)} | {m.group(1)} |")
    (app / "Contents/Resources/licenses/SHADERS.md").write_text(
        "# Shaders inside Nida.app\n\n"
        "Varda's and Nida's own, under the MIT licence, except as credited here.\n"
        "Shaders whose licence is non-commercial or not stated are left out of\n"
        "this app (scripts/ci/shaders-left-out.txt); they stay in the source.\n\n"
        "| Shader | Credit |\n|---|---|\n" + "\n".join(rows) + "\n"
    )


CORE = "https://raw.githubusercontent.com/Homebrew/homebrew-core"
# A tap of our own, there only while the sources are fetched.
TAP = ("nidasources", "pinned")


def fetch_url(url):
    try:
        with urllib.request.urlopen(url, timeout=120) as r:
            return r.read()
    except OSError as e:
        sys.exit(f"could not fetch {url}: {e}")


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def mac_resources(ruby):
    """The resources a formula declares that Apple silicon macOS fetches:
    none inside a head, on_linux or on_intel block, a test block or a
    method (run later, if ever, not when the formula is read), nor under a
    condition this does not judge (another on_… block). Formulae are laid
    out by rubocop, so each block's opener is the nearest line above that
    is indented less."""
    lines = ruby.splitlines()
    names = []
    for i, line in enumerate(lines):
        # `resource "x", :test do` is fetched for `brew test` only.
        m = re.match(r'^(\s*)resource "([^"]+)"(\s*,\s*:(\w+))? do\b', line)
        if not m or m.group(4) == "test":
            continue
        indent, fetched = len(m.group(1)), True
        for prev in reversed(lines[:i]):
            stripped = prev.strip()
            if not stripped or stripped.startswith("#"):
                continue
            depth = len(prev) - len(prev.lstrip())
            if depth >= indent:
                continue
            indent = depth
            word = stripped.split()[0]
            opens = re.search(r"\bdo(\s*\|[^|]*\|)?$", stripped)
            if word == "def" or (opens and (
                word in ("head", "on_linux", "on_intel", "test")
                or (word.startswith("on_") and word not in ("on_macos", "on_arm"))
            )):
                fetched = False
                break
            if depth == 0:
                break
        if fetched:
            names.append(m.group(2))
    return names


def sources(packages, info, tar_path, version):
    """The exact source of every bundled Homebrew package — main source,
    resources and patches — the formula that built it and its install
    receipt, in one archive for the release.

    Fetched through a tap of our own holding the formulae as homebrew-core
    had them (`tap_git_head`): a formula read from Homebrew's API data has
    no resources, and `brew fetch` would leave them out. Then checked
    against what each formula asks for: the main source's checksum (or git
    revision), each patch's checksum, each resource Apple silicon gets."""
    for name, (_, installed, _) in sorted(packages.items()):
        if keg_version(info[name]) != installed:
            sys.exit(f"{name} {installed} is installed but its formula is now "
                     f"{keg_version(info[name])}: reinstall it, so the source matches")
    stage = Path(tempfile.mkdtemp(prefix="nida-sources-"))
    top = stage / f"Nida-{version}-third-party-source"
    for folder in ("sources", "formulae", "patches", "receipts"):
        (top / folder).mkdir(parents=True)
    for name in sorted(packages):
        f = info[name]
        head, path = f.get("tap_git_head"), f.get("ruby_source_path")
        if not (head and path):
            sys.exit(f"{name}: Homebrew names no formula source to fetch it by")
        (top / "formulae" / f"{name}.rb").write_bytes(fetch_url(f"{CORE}/{head}/{path}"))
        for patch in f.get("patches") or []:
            if patch.get("file"):  # kept in homebrew-core itself
                kept = top / "patches" / patch["file"]
                kept.parent.mkdir(parents=True, exist_ok=True)
                kept.write_bytes(fetch_url(f"{CORE}/{head}/{patch['file']}"))

    tap = Path(run("brew", "--repository").strip()) / "Library/Taps" / TAP[0] / f"homebrew-{TAP[1]}"
    if tap.parent.exists():
        sys.exit(f"{tap.parent} is there already: remove it first")
    # A download cache of its own, so exactly what these formulae build from
    # lands in it — but the formula data `brew info` read.
    cache = stage / "cache"
    cache.mkdir()
    (cache / "api").symlink_to(Path(run("brew", "--cache").strip()) / "api")
    env = dict(os.environ, HOMEBREW_CACHE=str(cache), HOMEBREW_NO_AUTO_UPDATE="1",
               HOMEBREW_API_AUTO_UPDATE_SECS="1000000")
    try:
        (tap / "Formula").mkdir(parents=True)
        for formula in sorted((top / "formulae").iterdir()):
            # What only official taps may say: how Homebrew bumps versions,
            # nothing about what a formula builds or fetches.
            ruby = re.sub(r"(?m)^\s*no_autobump!.*\n", "", formula.read_text())
            (tap / "Formula" / formula.name).write_text(ruby)
        shutil.copytree(top / "patches", tap, dirs_exist_ok=True)
        run("brew", "fetch", "--build-from-source", "--retry", "--formula",
            *(f"{TAP[0]}/{TAP[1]}/{name}" for name in sorted(packages)), env=env)
    finally:
        shutil.rmtree(tap.parent, ignore_errors=True)

    downloads = cache / "downloads"
    sums = {sha256(p) for p in downloads.iterdir() if p.is_file() and not p.is_symlink()}
    for name in sorted(packages):
        f = info[name]
        stable = f.get("urls", {}).get("stable") or {}
        if stable.get("revision") or stable.get("url", "").endswith(".git"):
            checkout = cache / f"{name}--git"
            head = run("git", "-C", str(checkout), "rev-parse", "HEAD").strip() if checkout.is_dir() else ""
            if not head or (stable.get("revision") and head != stable["revision"]):
                sys.exit(f"{name}: its git source was not fetched at {stable.get('revision')}")
        elif stable.get("checksum") not in sums:
            sys.exit(f"{name}: no download has its source's checksum")
        for patch in f.get("patches") or []:
            if patch.get("url") and patch.get("sha256") not in sums:
                sys.exit(f"{name}: its patch {patch['url']} was not fetched")
        # Homebrew names each resource it fetched <formula>--<resource>--…
        # in its cache: a link to the download, or a git checkout.
        for resource in mac_resources((top / "formulae" / f"{name}.rb").read_text()):
            if not any(e.name.startswith(f"{name}--{resource}--") and e.exists() for e in cache.iterdir()):
                sys.exit(f"{name}: its resource {resource} was not fetched")

    # Homebrew's own: formula data, its Ruby boot cache, the formulae it
    # read (formulae/ has them from homebrew-core).
    for entry in sorted(cache.iterdir()):
        if entry.name in ("api", "api-source", "bootsnap") or entry.is_symlink():
            continue
        if entry.name == "downloads":
            for file in sorted(entry.iterdir()):
                if not file.is_symlink() and file.suffix != ".rb":
                    shutil.move(str(file), str(top / "sources" / file.name))
        elif entry.is_dir():  # a git checkout: its files at that revision
            shutil.copytree(entry, top / "sources" / entry.name, ignore=shutil.ignore_patterns(".git"))
        else:
            shutil.move(str(entry), str(top / "sources" / entry.name))
    rows = []
    for name in sorted(packages):
        _, installed, keg = packages[name]
        f = info[name]
        receipt = keg / "INSTALL_RECEIPT.json"
        if receipt.is_file():
            shutil.copyfile(receipt, top / "receipts" / f"{name}.json")
        rows.append(f"| {name} | {installed} | {f.get('license') or '?'} | {source_url(f)} |")
    heads = sorted({info[n]["tap_git_head"] for n in packages})
    (top / "README.md").write_text(
        f"# Source of the libraries inside Nida.app {version}\n\n"
        "Nida.app carries FFmpeg (GPL version 3 or later) and the libraries below,\n"
        "copied from Homebrew, so the app as a whole is given away under the GPL\n"
        "version 3. This archive is their corresponding source:\n\n"
        "- `sources/`: each package's source exactly as Homebrew fetched it to build\n"
        "  that version — main source, resources (a git one as its files at the\n"
        "  formula's revision) and downloaded patches;\n"
        "- `patches/`: the patches homebrew-core keeps itself;\n"
        "- `formulae/<name>.rb`: the Homebrew formula that built it (homebrew-core at\n"
        f"  {', '.join(heads)});\n"
        "- `receipts/<name>.json`: Homebrew's install receipt (options, dependencies).\n\n"
        f"Nida's own source, with the code it vendors (vendor/), is the repository at\n"
        f"tag v{version} ({REPO}/tree/v{version}, and the Source code archives on the\n"
        "release page); the other Rust crates built into it are on crates.io at the\n"
        "versions its Cargo.lock names (`cargo fetch` downloads them).\n"
        "scripts/ci/build-macos.sh builds the app from all of it.\n\n"
        "| Package | Version | Licence | Published at |\n|---|---|---|---|\n" + "\n".join(rows) + "\n"
    )
    with tarfile.open(tar_path, "w:gz") as tar:
        tar.add(top, arcname=top.name)
    shutil.rmtree(stage, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("app", type=Path)
    parser.add_argument("--min-macos", required=True)
    parser.add_argument("--identifier", default="lt.proconf.nida")
    parser.add_argument("--origin", action="append", default=[], metavar="NAME=PATH")
    parser.add_argument("--cargo-args")
    parser.add_argument("--version", default="dev")
    parser.add_argument("--sources", type=Path)
    args = parser.parse_args()
    app = args.app.resolve()
    origins = dict(o.split("=", 1) for o in args.origin)
    named = bundle(app, args.min_macos, args.identifier, origins)
    packages, info = homebrew_packages(named, origins)
    licences(app, packages, info, args.version)
    crates = rust_licences(app, args.cargo_args) if args.cargo_args is not None else 0
    shader_credits(app)
    # The licence files went in after signing: sign the app again over them.
    run("codesign", "--force", "--sign", "-", "--identifier", args.identifier, str(app))
    run("codesign", "--verify", "--deep", "--strict", str(app))
    if args.sources:
        sources(packages, info, args.sources, args.version)
    print(f"bundled {len(named)} libraries from {len(packages)} Homebrew packages; {crates} crates")


if __name__ == "__main__":
    main()
