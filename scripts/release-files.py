"""Collect only verified packages into a new draft-release directory."""
import sys,pathlib,shutil,hashlib,re,json,subprocess
import release
root=pathlib.Path(sys.argv[1]);destination=pathlib.Path(sys.argv[2]);tag=sys.argv[3] if len(sys.argv)>3 else None
if tag:release.require(re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+',tag),'release tag must be vMAJOR.MINOR.PATCH')
archives=sorted(root.rglob('preflight-[0-9]*.tar.gz'));release.require(bool(archives),'no server packages')
prepared={};platforms=[];installer=None;macos=[]
for archive in archives:
 checksum=archive.with_name(archive.name+'.sha256');sha=checksum.read_text().split()[0];manifest=release.verify(archive,sha);release.require(manifest['component']=='preflight','server package required')
 if tag:release.require(tag=='v'+manifest['metadata']['version'],'tag differs from packaged version')
 release.require(archive.name not in prepared,'duplicate release filename');prepared[archive.name]=archive;prepared[checksum.name]=checksum
 candidate=archive.parent/'install-preflight.py';content=candidate.read_bytes();release.require(installer is None or installer==content,'platform installers differ');installer=content
 platforms.append(manifest['metadata']['os']+' '+manifest['metadata']['arch'])
 if manifest['metadata']['os']=='macos' and manifest['metadata']['arch']=='aarch64':macos.append((archive,sha,manifest['metadata']['version']))
release.require(len(macos)<=1,'duplicate macOS ARM64 package')
destination.mkdir(exist_ok=False)
for name,source in prepared.items():shutil.copyfile(source,destination/name)
(destination/'install-preflight.py').write_bytes(installer)
shutil.copyfile(pathlib.Path(__file__).resolve().parents[1]/'install.sh',destination/'install.sh')
plugin_version=json.loads((pathlib.Path(__file__).resolve().parents[1]/'integrations/codex/.codex-plugin/plugin.json').read_text())['version']
subprocess.run([sys.executable,str(pathlib.Path(__file__).with_name('package-codex-plugin.py')),str(destination/('preflight-codex-plugin-'+plugin_version+'.zip')),'--zip'],check=True,stdout=subprocess.DEVNULL)
if macos:
 archive,sha,version=macos[0]
 release.formula(archive,sha,'https://github.com/MaxLaneDev/preflight/releases/download/v'+version+'/'+archive.name,'https://github.com/MaxLaneDev/preflight',destination/'preflight.rb')
files=sorted(p for p in destination.iterdir() if p.is_file());(destination/'SHA256SUMS').write_text(''.join(hashlib.sha256(p.read_bytes()).hexdigest()+'  '+p.name+'\n' for p in files))
(destination/'RELEASE_NOTES.md').write_text('Preflight includes the Web UI and MCP in one server.\n\nBuilt and checked on:\n'+''.join('- '+p+'\n' for p in platforms)+'\nCheck SHA256SUMS before installation. Existing services are never restarted by the installer. Configuration and data stay outside the binary installation.\n\nWindows/x86_64 and NAT traversal remain outside this release unless separately accepted.\n')
print(json.dumps({'packages':len(archives),'platforms':platforms,'draft_only':True}))
