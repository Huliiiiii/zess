import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path


TARGETS = {
    "x86_64-unknown-linux-gnu": "zess-server-x86_64-unknown-linux-gnu.tar.gz",
    "x86_64-pc-windows-msvc": "zess-server-x86_64-pc-windows-msvc.exe",
}


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def debian_owners(output, candidate):
    owners = []
    for line in output.splitlines():
        match = re.fullmatch(
            r"([a-z0-9][a-z0-9+.-]*(?::[a-z0-9-]+)?"
            r"(?:, [a-z0-9][a-z0-9+.-]*(?::[a-z0-9-]+)?)*)"
            r": (/.+)",
            line,
        )
        if match and match.group(2) == str(candidate):
            owners.extend(name.split(":")[0] for name in match.group(1).split(", "))
    return owners


def linux_bundle(binary, destination, patchelf, licenses):
    program_headers = subprocess.check_output(["readelf", "-l", str(binary)], text=True)
    match = re.search(r"Requesting program interpreter:\s*(\S+)\]", program_headers)
    if not match or Path(match.group(1)).name != "ld-linux-x86-64.so.2":
        raise ValueError("expected x86_64 glibc interpreter")
    headers = subprocess.check_output(["readelf", "-l", str(patchelf)], text=True)
    dynamic = subprocess.check_output(["readelf", "-d", str(patchelf)], text=True)
    if re.search(r"\bINTERP\b", headers) or "(NEEDED)" in dynamic:
        raise ValueError("patchelf must be statically linked")
    subprocess.run([str(patchelf.resolve()), "--version"], check=True, stdout=subprocess.DEVNULL)

    dependencies = subprocess.check_output(["ldd", str(binary.resolve())], text=True)
    libraries = {"ld-linux-x86-64.so.2": Path(match.group(1))}
    for line in dependencies.splitlines():
        line = line.strip()
        if "not found" in line:
            raise ValueError(f"missing dynamic dependency: {line}")
        match = re.match(r"(\S+)\s+=>\s+(/\S+)", line)
        if match and "/" not in match.group(1):
            libraries[match.group(1)] = Path(match.group(2))

    if not licenses.is_dir() or not any(licenses.iterdir()):
        raise ValueError("runtime and patchelf license files are required")
    with tempfile.TemporaryDirectory(prefix="zess-server-package-") as temporary:
        root = Path(temporary)
        (root / "lib").mkdir()
        shutil.copy2(binary, root / "zess")
        shutil.copy2(patchelf, root / "patchelf")
        for name, path in libraries.items():
            shutil.copy2(path.resolve(), root / "lib" / name)
        shutil.copytree(licenses, root / "licenses")
        if os.environ.get("ZESS_DEBIAN_LICENSES") == "1":
            for path in libraries.values():
                for candidate in (path, path.resolve()):
                    result = subprocess.run(
                        ["dpkg-query", "-S", str(candidate)],
                        capture_output=True,
                        text=True,
                        env={**os.environ, "LC_ALL": "C", "LANGUAGE": "C"},
                    )
                    owners = debian_owners(result.stdout, candidate)
                    if result.returncode == 0 and owners:
                        break
                else:
                    raise ValueError(f"cannot find package owning {path}")
                for package in owners:
                    copyright = Path("/usr/share/doc") / package / "copyright"
                    if not copyright.is_file():
                        raise ValueError(f"missing copyright information for {package}")
                    shutil.copyfile(copyright, root / "licenses" / f"{package}-copyright")
        with tarfile.open(destination, "w:gz") as archive:
            for path in sorted(root.iterdir()):
                archive.add(path, arcname=path.name)


def package(target, binary, output, patchelf=None, licenses=None):
    output.mkdir(parents=True, exist_ok=True)
    version = subprocess.check_output([str(binary.resolve()), "--version"], text=True).strip()
    expected_version = os.environ["ZESS_RELEASE_VERSION"]
    if version != f"zess {expected_version}":
        raise ValueError(f"expected zess {expected_version}, got {version!r}")
    file = TARGETS[target]
    destination = output / file
    if target == "x86_64-unknown-linux-gnu":
        if patchelf is None or licenses is None:
            raise ValueError("Linux package requires patchelf and license directory")
        linux_bundle(binary, destination, patchelf, licenses)
    else:
        shutil.copyfile(binary, destination)
    metadata = {"version": expected_version, "target": target, "file": file, "sha256": sha256(destination)}
    (output / "metadata.json").write_text(json.dumps(metadata) + "\n", encoding="utf-8")


def manifest(artifacts, output, version):
    output.mkdir(parents=True, exist_ok=True)
    entries = []
    for target, filename in TARGETS.items():
        directory = artifacts / target
        metadata = json.loads((directory / "metadata.json").read_text(encoding="utf-8"))
        if metadata["target"] != target or metadata["file"] != filename:
            raise ValueError(f"artifact metadata does not match {target}")
        if metadata["version"] != version:
            raise ValueError(f"expected version {version} for {target}, got {metadata['version']}")
        binary = directory / filename
        if sha256(binary) != metadata["sha256"]:
            raise ValueError(f"SHA-256 mismatch for {filename}")
        shutil.copyfile(binary, output / filename)
        entries.append({"target": target, "file": filename, "sha256": metadata["sha256"]})
    (output / "manifest.json").write_text(
        json.dumps({"version": version, "artifacts": entries}, indent=2) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    if len(sys.argv) in (5, 7) and sys.argv[1] == "package":
        extra = [Path(value) for value in sys.argv[5:]]
        package(sys.argv[2], Path(sys.argv[3]), Path(sys.argv[4]), *extra)
    elif len(sys.argv) == 5 and sys.argv[1] == "manifest":
        manifest(Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4])
    else:
        sys.exit("usage: server-release.py package TARGET BINARY OUTPUT [PATCHELF LICENSES] | manifest ARTIFACTS OUTPUT VERSION")
