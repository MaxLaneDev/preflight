"""Local reproducible packages and user-owned binary activation. Never manages a server process or database."""
import argparse, gzip, hashlib, io, json, os, pathlib, platform, re, subprocess, sys, tarfile, tempfile
ROOT = pathlib.Path(__file__).resolve().parents[1]
MAX_FILE = 256 * 1024 * 1024

def require(condition, message):
    if not condition:
        raise ValueError(message)

def digest(path):
    h = hashlib.sha256()
    with open(path, 'rb') as source:
        for block in iter(lambda : source.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()

def metadata(binary):
    result = subprocess.run([str(binary.resolve()), '--version'], check=True, capture_output=True, text=True, timeout=10)
    return json.loads(result.stdout)

def licenses():
    target = next(line.split(': ',1)[1] for line in subprocess.check_output(['rustc','-vV'],text=True).splitlines() if line.startswith('host: '))
    result = subprocess.run(['cargo', 'metadata', '--locked', '--offline', '--filter-platform',target,'--format-version', '1'], cwd=ROOT, check=True, capture_output=True, text=True, timeout=60)
    metadata = json.loads(result.stdout)
    nodes = {node['id']:node for node in metadata['resolve']['nodes']}
    selected = set()
    pending = list(metadata['workspace_members'])
    while pending:
        item = pending.pop()
        if item in selected: continue
        selected.add(item)
        pending.extend(dep['pkg'] for dep in nodes[item]['deps'] if any(kind['kind']!='dev' for kind in dep['dep_kinds']))
    files = {}
    rows = []
    for package in sorted(metadata['packages'], key=lambda p: p['id']):
        if package['source'] is None or package['id'] not in selected:
            continue
        parent = pathlib.Path(package['manifest_path']).parent
        found = []
        candidates = set()
        for path in parent.rglob('*'):
            if re.match(r'(?i)^(license|licence|copying|notice)',path.name):candidates.add(path)
        if package.get('license_file'):
            candidates.add(parent / package['license_file'])
        for path in sorted(candidates):
            if not path.is_file() or path.is_symlink() or path.stat().st_size > 128 * 1024:
                continue
            if not path.resolve().is_relative_to(parent.resolve()):
                continue
            name = 'licenses/cargo/' + package['name'] + '-' + package['version'] + '/' + path.relative_to(parent).as_posix()
            files[name] = path
            found.append(name)
        rows.append({'name': package['name'], 'version': package['version'], 'license': package['license'], 'files': found})
    return (files, rows)

def installer_source():
    library = pathlib.Path(__file__).read_text().split("\nif __name__ == '__main__':\n", 1)[0]
    return '#!/usr/bin/env python3\n' + library + (ROOT/'scripts/installer-entry.py').read_text()

def formula(archive, expected, url, homepage, destination):
    manifest = verify(archive, expected)
    require(manifest['component'] == 'preflight', 'formula requires unified server package')
    require(manifest['metadata']['os'] == 'macos' and manifest['metadata']['arch'] == 'aarch64', 'current formula targets verified macOS ARM64 packages only')
    for value in [url, homepage]:
        require(re.fullmatch(r'https://[A-Za-z0-9./_%~:+-]+', value), 'formula URLs must be explicit safe HTTPS URLs')
    version = manifest['metadata']['version']
    require(re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?', version), 'invalid formula version')
    content = (ROOT/'packaging/preflight.rb.in').read_text()
    for key, value in {'URL':url,'HOMEPAGE':homepage,'VERSION':version,'SHA256':expected}.items():
        content = content.replace('@'+key+'@', value)
    with destination.open('x') as output: output.write(content)
    return {'formula': str(destination), 'sha256': digest(destination), 'publication': 'not performed', 'platform': manifest['metadata']['os']+'-'+manifest['metadata']['arch']}

def package(server, client, destination):
    destination.mkdir(parents=True, exist_ok=False)
    (license_files, notices) = licenses()
    archives = []
    for (binary, component) in [(server, 'preflight'), (client, 'preflight-client')]:
        if binary is None: continue
        info = metadata(binary)
        require(info['component'] == component, 'component mismatch')
        require(re.fullmatch('[0-9]+\\.[0-9]+\\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?', info['version']), 'invalid package version')
        require(re.fullmatch('[a-z0-9_]+', info['os']) and re.fullmatch('[a-z0-9_]+', info['arch']), 'invalid package or installation')
        name = component + ('.exe' if info['os'] == 'windows' else '')
        files = {name: binary, 'LICENSE': ROOT / 'LICENSE', **license_files}
        text = {'THIRD_PARTY_NOTICES.json': json.dumps(notices, indent=2).encode(), 'README.md': (('Preflight web server with bundled Web UI and MCP\n\nUse an external configuration file: preflight --config /absolute/preflight.toml serve.\n' if component == 'preflight' else 'Compatibility Preflight client\n\npreflight-client --server http://127.0.0.1:4170 --bind 127.0.0.1:4172\nAdd --token-env PREFLIGHT_API_TOKEN when API credentials are enabled.\n') + '\nNo process starts during installation. Keep configuration and data outside the package.\nUse release.py verify/install/rollback with the published archive SHA-256.\nThese local development artifacts are unsigned; no signing or platform acceptance is implied.\n').encode()}
        if component == 'preflight':
            text['install-preflight.py'] = installer_source().encode()
            text['README.md'] = b'''Preflight Web UI + MCP

Run: preflight --config /absolute/preflight.toml serve
Web UI: http://127.0.0.1:4170/
MCP: http://127.0.0.1:4170/mcp

Install with the included standalone Python 3.9+ script:
python3 install-preflight.py install ARCHIVE --sha256 SHA256 --prefix BINARIES --config CONFIG --data DATA
Use --help for status, rollback and uninstall. Keep configuration and data outside BINARIES.
Installation never starts or restarts a process. Stop active work before a deliberate restart.
Binary rollback does not undo database migrations; back up data before schema upgrades.
Dependency notices and licenses are included. Public publication/signing are separate release gates.
'''

        component_notices = list(notices)
        if component in ['preflight', 'preflight-client']:
            js = json.loads((ROOT/'crates/preflight-client/THIRD_PARTY_NOTICES.json').read_text())
            require(js['lock_sha256']==digest(ROOT/'crates/preflight-client/package-lock.json') and js['bundle_sha256']==digest(ROOT/'crates/preflight-client/src/editor-bundle.mjs'),'regenerate client notices after changing the bundle or lock')
            text.update({name:value.encode() for name,value in js['files'].items()})
            component_notices.extend(js['packages'])
        text['THIRD_PARTY_NOTICES.json']=json.dumps(component_notices,indent=2).encode()
        entries = {key: {'sha256': digest(path), 'size': path.stat().st_size, 'executable': key == name} for (key, path) in files.items()}
        entries.update({key: {'sha256': hashlib.sha256(value).hexdigest(), 'size': len(value), 'executable': False} for (key, value) in text.items()})
        manifest = {'format': 'preflight-package-v1', 'component': component, 'binary': name, 'metadata': info, 'packaging_lock_sha256': digest(ROOT / 'Cargo.lock'), 'files': entries}
        archive = destination / (component + '-' + info['version'] + '-' + info['os'] + '-' + info['arch'] + '.tar.gz')
        with open(archive, 'wb') as output, gzip.GzipFile(fileobj=output, mode='wb', filename='', mtime=0) as zipped, tarfile.open(fileobj=zipped, mode='w') as tar:
            payload = json.dumps(manifest, sort_keys=True, separators=(',', ':')).encode()
            header = tarfile.TarInfo('manifest.json')
            header.size = len(payload)
            header.mode = 420
            tar.addfile(header, io.BytesIO(payload))
            for key in sorted(entries):
                header = tarfile.TarInfo(key)
                header.size = entries[key]['size']
                header.mode = 493 if entries[key]['executable'] else 420
                if key in text:
                    tar.addfile(header, io.BytesIO(text[key]))
                else:
                    with open(files[key], 'rb') as source:
                        tar.addfile(header, source)
        sha = digest(archive)
        archive.with_name(archive.name + '.sha256').write_text(sha + '  ' + archive.name + '\n')
        archives.append({'archive': str(archive), 'sha256': sha, 'metadata': info})
    (destination / 'install-preflight.py').write_text(installer_source())
    return archives

def verify(archive, expected, destination=None):
    require(re.fullmatch('[a-f0-9]{64}', expected) and digest(archive) == expected, 'archive checksum mismatch')
    with tarfile.open(archive, 'r:gz') as tar:
        first = tar.next()
        require(first and first.name == 'manifest.json' and first.isfile() and (first.size <= 1024 * 1024), 'missing bounded manifest')
        manifest = json.loads(tar.extractfile(first).read())
        require(manifest['format'] == 'preflight-package-v1', 'invalid package or installation')
        entries = manifest['files']
        require(isinstance(entries, dict) and 1 <= len(entries) <= 4000, 'invalid package or installation')
        require(manifest['component'] in ['preflight', 'preflight-client'] and manifest['binary'] in entries, 'invalid package or installation')
        seen = set()
        total = 0
        seen_manifest = False
        for member in tar:
            if member.name == 'manifest.json':
                require(not seen_manifest, 'duplicate manifest')
                seen_manifest = True
                continue
            name = pathlib.PurePosixPath(member.name)
            require(not name.is_absolute() and '..' not in name.parts and ('\\' not in member.name) and (str(name) == member.name), 'unsafe archive path')
            require(not any(':' in part or part.endswith((' ', '.')) or any(ord(c)<32 or ord(c)==127 for c in part) or re.fullmatch(r'(?i)(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?',part) for part in name.parts),'unsafe Windows archive path')
            require(member.isfile() and member.name in entries and (member.name not in seen), 'unexpected archive entry')
            entry = entries[member.name]
            require(member.size == entry['size'] and 0 <= member.size <= MAX_FILE, 'invalid file size')
            total += member.size
            require(total <= 1024 * 1024 * 1024, 'package exceeds 1 GiB')
            seen.add(member.name)
            h = hashlib.sha256()
            target = None
            if destination:
                path = destination.joinpath(*name.parts)
                path.parent.mkdir(parents=True, exist_ok=True)
                target = open(path, 'xb')
            source = tar.extractfile(member)
            try:
                for block in iter(lambda : source.read(1024 * 1024), b''):
                    h.update(block)
                    if target:
                        target.write(block)
            finally:
                if target:
                    target.close()
            require(h.hexdigest() == entry['sha256'], 'payload checksum mismatch')
            if destination:
                os.chmod(path, 493 if entry['executable'] else 420)
        require(seen == set(entries), 'missing package files')
        return manifest

def state(root):
    record = root / '.preflight-install.json'
    require(record.is_file(), 'not a Preflight installation')
    data = json.loads(record.read_text())
    if os.name != 'nt':
        current = pathlib.Path(os.readlink(root / 'current')).name
        require(re.fullmatch('[a-f0-9]{64}', current), 'invalid activation pointer')
        if data['current'] != current:
            data['previous'] = data['current']
            data['current'] = current
    return data

def activate(root, data):
    current = root / 'current'
    if os.name != 'nt':
        require(not current.exists() or current.is_symlink(), 'current must be an owned activation symlink')
        temporary = root / ('.activate-' + os.urandom(12).hex())
        temporary.symlink_to(pathlib.Path('versions') / data['current'])
        os.replace(temporary, current)
    record = root / '.preflight-install.json'
    temporary = record.with_suffix('.new')
    temporary.write_text(json.dumps(data, indent=2) + '\n')
    os.replace(temporary, record)
    return {'component': data['component'], 'current': data['current'], 'binary': str(root / 'versions' / data['current'] / data['binary']), 'restart': 'not performed; existing processes and data are untouched'}

def install(archive, expected, root):
    manifest = verify(archive, expected)
    host_os = {'darwin': 'macos', 'win32': 'windows'}.get(sys.platform, sys.platform)
    host_arch = {'arm64': 'aarch64', 'AMD64': 'x86_64'}.get(platform.machine(), platform.machine())
    require(manifest['metadata']['os'] == host_os and manifest['metadata']['arch'] == host_arch, 'package platform does not match this host')
    require(not root.is_symlink(), 'installation root must not be a symlink')
    if root.exists():
        require((root / '.preflight-install.json').is_file() or not any(root.iterdir()), 'destination contains unrelated data')
    root.mkdir(parents=True, exist_ok=True)
    old = state(root) if (root / '.preflight-install.json').exists() else None
    if old:
        require(old['component'] == manifest['component'], 'cannot replace a different component')
    versions = root / 'versions'
    require(not versions.is_symlink(), 'versions must not be a symlink')
    versions.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.install-', dir=versions) as temporary:
        temporary = pathlib.Path(temporary)
        verify(archive, expected, temporary)
        (temporary / 'manifest.json').write_text(json.dumps(manifest))
        version = versions / expected
        if version.exists():
            require(version.is_dir() and (not version.is_symlink()), 'invalid installed version directory')
            for (name, entry) in manifest['files'].items():
                require(digest(version / name) == entry['sha256'], 'installed version was modified')
        else:
            os.replace(temporary, version)
    data = {'format': 'preflight-install-v1', 'component': manifest['component'], 'binary': manifest['binary'], 'current': expected, 'previous': old['current'] if old and old['current'] != expected else (old or {}).get('previous')}
    return activate(root, data)

def rollback(root):
    data = state(root)
    previous = data.get('previous')
    require(previous and (root / 'versions' / previous / 'manifest.json').is_file(), 'no previous version')
    manifest = json.loads((root / 'versions' / previous / 'manifest.json').read_text())
    for (name, entry) in manifest['files'].items():
        require(digest(root / 'versions' / previous / name) == entry['sha256'], 'previous version was modified')
    (data['current'], data['previous']) = (previous, data['current'])
    return activate(root, data)
if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='operation', required=True)
    p = sub.add_parser('package')
    p.add_argument('--server', type=pathlib.Path, required=True)
    p.add_argument('--client', type=pathlib.Path)
    p.add_argument('--destination', type=pathlib.Path, required=True)
    for operation in ['verify', 'install']:
        p = sub.add_parser(operation)
        p.add_argument('archive', type=pathlib.Path)
        p.add_argument('--sha256', required=True)
        if operation == 'install':
            p.add_argument('destination', type=pathlib.Path)
    p = sub.add_parser('formula')
    p.add_argument('archive', type=pathlib.Path)
    p.add_argument('--sha256', required=True)
    p.add_argument('--url', required=True)
    p.add_argument('--homepage', required=True)
    p.add_argument('--destination', type=pathlib.Path, required=True)
    p = sub.add_parser('rollback')
    p.add_argument('destination', type=pathlib.Path)
    args = parser.parse_args()
    if args.operation == 'package':
        result = package(args.server, args.client, args.destination)
    elif args.operation == 'formula':
        result = formula(args.archive, args.sha256, args.url, args.homepage, args.destination)
    elif args.operation == 'verify':
        result = verify(args.archive, args.sha256)
    elif args.operation == 'install':
        result = install(args.archive, args.sha256, args.destination)
    else:
        result = rollback(args.destination)
    print(json.dumps(result, sort_keys=True))
