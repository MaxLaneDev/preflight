# Appended to release.py's verified package implementation for a single-file installer.
def installer_main():
    import urllib.request
    import shutil
    parser = argparse.ArgumentParser(description='Install Preflight Web UI + MCP. Never restarts a running service.')
    sub = parser.add_subparsers(dest='operation', required=True)
    p = sub.add_parser('install')
    p.add_argument('source', help='Local release archive or HTTPS URL')
    p.add_argument('--sha256', required=True)
    p.add_argument('--prefix', type=pathlib.Path, required=True)
    p.add_argument('--config', type=pathlib.Path, required=True)
    p.add_argument('--data', type=pathlib.Path)
    for operation in ['status', 'rollback', 'uninstall']:
        p = sub.add_parser(operation)
        p.add_argument('--prefix', type=pathlib.Path, required=True)
    args = parser.parse_args()
    require(not args.prefix.is_symlink(), 'installation root must not be a symlink')
    root = args.prefix.resolve()
    if args.operation == 'install':
        require(not args.config.is_symlink(), 'configuration must not be a symlink')
        config = args.config.resolve()
        data = (args.data or config.parent / 'data').resolve()
        require(not config.is_relative_to(root) and not data.is_relative_to(root), 'configuration/data must be outside the binary installation')
        require(not config.is_symlink(), 'configuration must not be a symlink')
        with tempfile.TemporaryDirectory(prefix='preflight-download-') as temporary:
            source = pathlib.Path(args.source)
            if '://' in args.source:
                require(args.source.startswith('https://'), 'release downloads require HTTPS')
                class SecureRedirect(urllib.request.HTTPRedirectHandler):
                    def redirect_request(self, req, fp, code, msg, headers, newurl):
                        require(newurl.startswith('https://'), 'release redirect must use HTTPS')
                        return super().redirect_request(req, fp, code, msg, headers, newurl)
                source = pathlib.Path(temporary) / 'release.tar.gz'
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), SecureRedirect())
                with opener.open(args.source, timeout=60) as response, source.open('wb') as output:
                    total = 0
                    while block := response.read(1024 * 1024):
                        total += len(block)
                        require(total <= 512 * 1024 * 1024, 'release download exceeds 512 MiB')
                        output.write(block)
            manifest = verify(source, args.sha256)
            require(manifest['component'] == 'preflight', 'installer requires the unified server package')
            result = install(source, args.sha256, root)
        if not config.exists():
            config.parent.mkdir(parents=True, exist_ok=True)
            import secrets
            credential = config.with_name(config.stem + '.admin-token')
            with credential.open('x') as secret_file:
                os.chmod(credential, 0o600)
                secret_file.write(secrets.token_urlsafe(48) + '\n')
            result['admin_token_file'] = str(credential)
            with config.open('x') as output:
                os.chmod(config, 0o600)
                output.write('version = 1\nbind = "127.0.0.1:4170"\ndata_dir = '+json.dumps(str(data))+'\nallowed_roots = []\nadmin_token_file = '+json.dumps(str(credential))+'\n')
        result.update(configuration=str(config), start=[str(root/'current'/manifest['binary']), '--config', str(config), 'serve'], url='http://127.0.0.1:4170', mcp='http://127.0.0.1:4170/mcp')
    elif args.operation == 'status':
        result = state(root)
    elif args.operation == 'rollback':
        result = rollback(root)
    else:
        record = state(root)
        require(record['component'] == 'preflight' and not root.is_symlink(), 'not a unified server installation')
        require(set(p.name for p in root.iterdir()) == {'versions','current','.preflight-install.json'}, 'installation contains unrelated files; refusing uninstall')
        processes = subprocess.check_output(['ps','-ax','-o','command='], text=True)
        require(not any(line.strip().startswith((str(root/'current'/'preflight'), str(root/'versions')+'/')) for line in processes.splitlines()), 'stop the installed service before uninstalling')
        versions = root/'versions'
        require(not versions.is_symlink(), 'versions must not be a symlink')
        for directory in versions.iterdir():
            require(directory.is_dir() and not directory.is_symlink() and re.fullmatch('[a-f0-9]{64}',directory.name), 'unrecognized installed version')
            manifest = json.loads((directory/'manifest.json').read_text())
            expected = set(manifest['files']) | {'manifest.json'}
            actual = set(p.relative_to(directory).as_posix() for p in directory.rglob('*') if p.is_file())
            require(actual == expected and not any(p.is_symlink() for p in directory.rglob('*')), 'installed version contains unrelated files')
            for name, entry in manifest['files'].items():
                require(digest(directory/name) == entry['sha256'], 'installed version was modified')
        shutil.rmtree(root)
        result = {'uninstalled': True, 'configuration_and_data': 'preserved', 'service': 'not stopped or removed by installer'}
    print(json.dumps(result, sort_keys=True))

if __name__ == '__main__':
    installer_main()
